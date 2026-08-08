//! End-to-end TUI test: spawns the real `ga` binary inside a PTY,
//! drives it with keystrokes, and asserts the rendered frames have
//! no `<summary>` / `<thinking>` tag leaks and no overflow.
//!
//! This is the "real terminal" test: `portable-pty` gives us a
//! real pseudo-terminal (the same kind `tmux`/`sshd` use), so the
//! TUI goes through its full crossterm + ratatui render path.
//!
//! The test is gated behind the `GA_TUI_E2E` env var so it doesn't
//! run on every `cargo test` invocation — it needs a live LLM
//! endpoint and takes ~30s. Run with:
//!
//!     GA_TUI_E2E=1 cargo test -p ga-tui --test pty_e2e -- --nocapture
//!
//! Requires:
//!   - `ga` binary built (`cargo build --release --bin ga`)
//!   - LLM endpoint reachable per `config/mykey.toml`
//!   - A working directory with a few files (e.g. the project root)

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// Strip ANSI escape sequences to get the plain visible text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for inner in chars.by_ref() {
                if inner.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Like `strip_ansi`, but also converts cursor-movement and
/// screen-clear commands into newlines. This is needed because
/// the TUI redraws the full screen every animation tick — the
/// raw stream is many overlaid frames, and without splitting
/// at cursor moves the per-frame rows all collapse into one
/// line, breaking the overflow check.
fn strip_ansi_with_layout(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut command = None;
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphabetic() {
                        command = Some(next);
                        chars.next();
                        break;
                    } else {
                        chars.next();
                    }
                }
                match command {
                    Some('A') | Some('B') | Some('H') | Some('f') | Some('J') => {
                        out.push('\n');
                    }
                    Some('K') => {
                        out.push(' ');
                    }
                    _ => {}
                }
            } else {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Find the first `[section]` in a `key = "value"` TOML file.
/// Used to pick a valid session name without pulling in a TOML dep.
fn first_toml_section(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.starts_with("[[") {
            let name = &trimmed[1..trimmed.len() - 1];
            // skip [meta] sections like [package]
            if !name.contains('.') {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[test]
fn e2e_tui_no_tag_leak_and_no_overflow() {
    if std::env::var("OZ_TUI_E2E").is_err() {
        eprintln!("skipping e2e (set GA_TUI_E2E=1 to run)");
        return;
    }

    // ── 1. Locate the ga binary ──
    let exe = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/release/ga");
    let exe_str = exe.to_string_lossy().to_string();
    if !exe.exists() {
        panic!(
            "ga binary not found at {}. Build with `cargo build --release --bin ga`",
            exe.display()
        );
    }
    eprintln!("spawning: {}", exe_str);

    // ── 2. Discover a valid session from config/mykey.toml ──
    let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let cfg_path = workspace.join("config/mykey.toml");
    let session = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|c| first_toml_section(&c))
        .unwrap_or_else(|| "default".to_string());
    eprintln!("using session: {session} (from {})", cfg_path.display());

    // ── 3. Open a PTY and spawn the TUI ──
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(&exe_str);
    cmd.arg("tui");
    cmd.arg("-s");
    cmd.arg(&session);
    for (k, v) in std::env::vars() {
        if k != "RUST_LOG" {
            cmd.env(k, v);
        }
    }
    cmd.env("RUST_LOG", "warn");
    cmd.env("RUST_BACKTRACE", "0");
    cmd.cwd(&workspace);
    eprintln!("cwd: {}", workspace.display());

    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().expect("clone reader");
    let mut reader: Box<dyn Read + Send> = Box::new(reader);
    let mut writer: Box<dyn Write + Send> =
        Box::new(pair.master.take_writer().expect("take writer"));

    // ── 4. Spawn a background drain thread ──
    let collected = Arc::new(Mutex::new(Vec::<u8>::new()));
    let collected_writer = collected.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut buf = collected_writer.lock().unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > 1_000_000 {
                        let drop_to = buf.len() - 500_000;
                        buf.drain(..drop_to);
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Give the TUI a moment to draw its first frame.
    std::thread::sleep(Duration::from_millis(2000));

    // If the child already exited (e.g. wrong session, missing asset,
    // port already bound), the PTY slave is closed and our first
    // write will hit EIO. Detect that early and dump the captured
    // output so failures are diagnosable.
    if let Ok(Some(status)) = child.try_wait() {
        let raw = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
        panic!(
            "ga exited before test could drive it (status: {status:?}).\n--- captured PTY output ---\n{}\n--- end ---",
            strip_ansi(&raw)
        );
    }

    // ── 5. Send a complex task ──
    let task = "Read the file src/main.rs (max 20 lines), then list the crates in the workspace, and give me a 3-line summary of what the project does.";
    eprintln!("sending task: {task}");

    if let Err(e) = writer.write_all(b"i") {
        let raw = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
        panic!(
            "write i failed: {e}\n--- captured PTY output ---\n{}\n--- end ---",
            strip_ansi(&raw)
        );
    }
    writer.flush().ok();
    std::thread::sleep(Duration::from_millis(400));
    {
        let mut buf = collected.lock().unwrap();
        buf.clear();
    }
    // Type the task.
    if let Err(e) = writer.write_all(task.as_bytes()) {
        let raw = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
        panic!("write task failed: {e}\noutput:\n{}", strip_ansi(&raw));
    }
    writer.flush().ok();
    std::thread::sleep(Duration::from_millis(300));
    // Press Enter.
    if let Err(e) = writer.write_all(b"\r") {
        let raw = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
        panic!("write enter failed: {e}\noutput:\n{}", strip_ansi(&raw));
    }
    writer.flush().ok();

    // ── 6. Wait for activity, then stop the agent ──
    // We stop manually after the first tool call completes so
    // we don't wait minutes for a slow local model to finish
    // generating its full response.
    let start = Instant::now();
    let mut last_dump = Instant::now();
    let mut saw_activity = false;
    let activity_window = Duration::from_secs(30);
    while start.elapsed() < activity_window {
        std::thread::sleep(Duration::from_millis(500));
        let buf = collected.lock().unwrap();
        let text = strip_ansi(&String::from_utf8_lossy(&buf));
        if text.contains(" ✓") || text.contains("\u{2713}") {
            saw_activity = true;
            break;
        }
        if last_dump.elapsed() > Duration::from_secs(15) {
            eprintln!(
                "--- frame @ {:?} ---\n{}\n--- end frame ---",
                start.elapsed(),
                &text[..text.len().min(2000)]
            );
            last_dump = Instant::now();
        }
    }
    eprintln!("saw tool-call completion: {saw_activity}");

    if saw_activity {
        writer.write_all(b"s").expect("write s");
        writer.flush().ok();
        eprintln!("sent stop signal");
    }

    let done_window = Duration::from_secs(60);
    let done_start = Instant::now();
    let mut done = false;
    while done_start.elapsed() < done_window {
        std::thread::sleep(Duration::from_millis(500));
        let buf = collected.lock().unwrap();
        let text = strip_ansi(&String::from_utf8_lossy(&buf));
        if text.contains("Done (end_turn)")
            || text.contains("Done (stopped_by_user)")
            || text.contains("Done (error")
        {
            eprintln!("agent finished in {:?}", done_start.elapsed());
            done = true;
            break;
        }
    }
    if !done {
        eprintln!("warning: agent did not report Done within 60s of stop; asserting on output anyway");
    }

    // Give a final 800ms to flush any trailing frames.
    std::thread::sleep(Duration::from_millis(800));

    // ── 7. Press 'q' to quit ──
    writer.write_all(b"q").expect("write q");
    writer.flush().ok();
    std::thread::sleep(Duration::from_millis(300));
    writer.write_all(b"y").expect("write y");
    writer.flush().ok();
    std::thread::sleep(Duration::from_millis(800));

    // Stop the reader thread.
    drop(writer);
    let _ = child.wait();
    let _ = reader_thread.join();

    // ── 8. Assertions ──
    let raw = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
    let visible = strip_ansi(&raw);
    let layout = strip_ansi_with_layout(&raw);

    // 7a. No raw tag fragments in the visible output.
    for needle in [
        "<thinking>", "</thinking>", "<summary>", "</summary>", "<think>", "</think>",
    ] {
        assert!(
            !visible.contains(needle),
            "tag fragment {:?} leaked into visible output:\n{}",
            needle,
            &visible[..visible.len().min(3000)]
        );
    }

    // 7b. No line exceeds the PTY width (120 cols) by a wide
    // margin — this is the "tool call overflow" regression test.
    let mut max_line_len = 0usize;
    let mut offending = String::new();
    for line in layout.lines() {
        if line.chars().count() > max_line_len {
            max_line_len = line.chars().count();
            offending = line.to_string();
        }
    }
    assert!(
        max_line_len <= 130,
        "line exceeds 120 cols (len={}): {:?}",
        max_line_len,
        offending
    );

    // 7c. The user message and a tool call should be visible.
    assert!(
        visible.contains("Read the file src/main.rs"),
        "user message not echoed in output"
    );
    assert!(
        visible.contains("read") || visible.contains("read_file") || visible.contains("📖"),
        "no tool-call card visible in output"
    );

    // 7d. At least one thinking block should be visible.
    assert!(
        visible.contains("Thinking") || visible.contains("💭"),
        "no thinking card visible in output"
    );

    eprintln!(
        "PASS: no tag leaks, max line {} cols, all expected cards present",
        max_line_len
    );
}
