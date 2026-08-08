use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};

use nix::pty;
use nix::sys::signal::{self, Signal};
use nix::sys::wait::WaitStatus;
use nix::unistd::Pid;
use tauri::{AppHandle, Emitter};
pub struct TerminalSession {
    pub pid: u32,
    pub master_fd: OwnedFd,
    pub cwd: String,
    pub shell: String,
    pub exited: bool,
    pub exit_code: Option<i32>,
}

pub type TerminalRegistry = Arc<Mutex<HashMap<String, TerminalSession>>>;

pub fn spawn_terminal(
    app: AppHandle,
    registry: TerminalRegistry,
    session_id: String,
    shell: Option<String>,
    cwd: Option<String>,
) -> Result<String, String> {
    eprintln!(
        "[sidepanel::terminal] spawn_terminal called: session_id={}, shell={:?}, cwd={:?}",
        session_id, shell, cwd
    );

    let shell_path = shell.unwrap_or_else(|| {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
    });
    let workdir = cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/".into())
    });

    eprintln!(
        "[sidepanel::terminal] using shell={}, workdir={}",
        shell_path, workdir
    );

    let pty_pair = pty::openpty(None, None)
        .map_err(|e| {
            eprintln!("[sidepanel::terminal] openpty failed: {e}");
            format!("openpty failed: {e}")
        })?;

    let master_raw = pty_pair.master.into_raw_fd();
    let slave_raw = pty_pair.slave.into_raw_fd();
    eprintln!(
        "[sidepanel::terminal] openpty OK: master_raw={}, slave_raw={}",
        master_raw, slave_raw
    );

    let winsize = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    // TIOCSWINSZ on a PTY master fd we just created; failure only affects
    // initial row/col size, so the ioctl result is intentionally not checked
    // here (safe: fd is owned, in-bounds struct).
    unsafe { libc::ioctl(master_raw, libc::TIOCSWINSZ, &winsize); }

    eprintln!("[sidepanel::terminal] calling fork…");
    let fork_res = unsafe {
        nix::unistd::fork().map_err(|e| {
            eprintln!("[sidepanel::terminal] fork failed: {e}");
            format!("fork failed: {e}")
        })?
    };

    match fork_res {
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::setsid();
            // Unsafe block: child-side PTY setup after fork. Required to make
            // the slave the controlling tty and wire stdio to it; all fds are
            // the ones produced by openpty in this function, so the raw-fd
            // conversions are valid. Error results are ignored here because
            // `exec` below is the definitive failure point for the child.
            unsafe { libc::ioctl(slave_raw, libc::TIOCSCTTY.into(), 0); }
            // Unsafe block: child-side stdio wiring. dup2 is required to
            // attach the PTY slave to stdin/stdout/stderr before exec; the
            // raw fds come from openpty above and are valid in the child
            // after fork. Errors are ignored because exec is the definitive
            // failure point.
            let _ = nix::unistd::dup2(slave_raw, 0);
            let _ = nix::unistd::dup2(slave_raw, 1);
            let _ = nix::unistd::dup2(slave_raw, 2);
            if slave_raw > 2 { let _ = nix::unistd::close(slave_raw); }
            let _ = nix::unistd::close(master_raw);

            std::env::set_var("TERM", "xterm-256color");
            std::env::set_var("PROMPT_EOL_MARK", "");
            std::env::set_var("ZSH_DISABLE_COMPFIX", "true");
            if let Ok(p) = CString::new(workdir.clone()) { let _ = nix::unistd::chdir(p.as_c_str()); }

            // Force interactive shell so the prompt is emitted even when
            // stdin is non-tty (defensive — we did dup2 the slave PTY).
            let err = Command::new(&shell_path).arg("-i").exec();
            let _ = nix::unistd::write(2, format!("exec failed: {err}\n").as_bytes());
            std::process::exit(1);
        }
        nix::unistd::ForkResult::Parent { child } => {
            let _ = nix::unistd::close(slave_raw);
            let pid = child.as_raw() as u32;
            eprintln!(
                "[sidepanel::terminal] parent PID={} child={} master_raw={}",
                std::process::id(), pid, master_raw
            );

            eprintln!("[sidepanel::terminal] STEP: creating master_owned from raw fd");
            // OwnedFd::from_raw_fd: takes ownership of master_raw, which we
            // obtained via into_raw_fd above — no double-close, fd is owned
            // by this process.
            let master_owned = unsafe { OwnedFd::from_raw_fd(master_raw) };

            eprintln!("[sidepanel::terminal] STEP: building TerminalSession");
            let session = TerminalSession {
                pid,
                master_fd: master_owned,
                cwd: workdir.clone(),
                shell: shell_path.clone(),
                exited: false,
                exit_code: None,
            };

            eprintln!("[sidepanel::terminal] STEP: locking registry to insert session");
            {
                let mut sessions = registry.lock().unwrap();
                sessions.insert(session_id.clone(), session);
            }
            eprintln!("[sidepanel::terminal] STEP: session inserted, cloning handles");

            let app2 = app.clone();
            let reg2 = registry.clone();
            let sid2 = session_id.clone();

            eprintln!("[sidepanel::terminal] STEP: spawning reader std::thread");
            std::thread::spawn(move || {
                eprintln!(
                    "[sidepanel::terminal] read loop thread STARTED for sid={}, master_raw={}",
                    sid2, master_raw
                );
                let mut buf = [0u8; 4096];
                let mut any_read = false;
                loop {
                    match nix::unistd::read(master_raw, &mut buf) {
                        Ok(0) => {
                            eprintln!(
                                "[sidepanel::terminal] read returned 0 (EOF) for sid={}",
                                sid2
                            );
                            break;
                        }
                        Ok(n) => {
                            if !any_read {
                                eprintln!(
                                    "[sidepanel::terminal] first read OK: {} bytes for sid={}",
                                    n, sid2
                                );
                                any_read = true;
                            }
                            let data = String::from_utf8_lossy(&buf[..n]).to_string();
                            match app2.emit("terminal:data", serde_json::json!({
                                "session_id": sid2, "data": data,
                            })) {
                                Ok(_) => {}
                                Err(e) => eprintln!(
                                    "[sidepanel::terminal] emit terminal:data FAILED: sid={}, err={}",
                                    sid2, e
                                ),
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "[sidepanel::terminal] read error: sid={}, err={}",
                                sid2, e
                            );
                            let _ = app2.emit("terminal:data", serde_json::json!({
                                "session_id": sid2,
                                "data": format!("\r\n[read error: {e}]\r\n"),
                            }));
                            break;
                        }
                    }

                    let sessions = reg2.lock().unwrap();
                    if let Some(s) = sessions.get(&sid2) {
                        if s.exited { break; }
                    } else { break; }
                }

                let exit_code = match nix::sys::wait::waitpid(
                    Pid::from_raw(pid as i32),
                    Some(nix::sys::wait::WaitPidFlag::WNOHANG),
                ) {
                    Ok(WaitStatus::Exited(_, c)) => Some(c),
                    Ok(WaitStatus::Signaled(_, s, _)) => Some(-(s as i32)),
                    _ => None,
                };
                eprintln!(
                    "[sidepanel::terminal] read loop EXITED: sid={}, exit_code={:?}",
                    sid2, exit_code
                );

                {
                    let mut sessions = reg2.lock().unwrap();
                    if let Some(s) = sessions.get_mut(&sid2) {
                        s.exited = true;
                        s.exit_code = exit_code;
                    }
                }

                let _ = app2.emit("terminal:exited", serde_json::json!({
                    "session_id": sid2, "exit_code": exit_code,
                }));
            });

            eprintln!(
                "[sidepanel::terminal] emitting terminal:created: session_id={}, shell={}, cwd={}, pid={}",
                session_id, shell_path, workdir, pid
            );
            let _ = app.emit("terminal:created", serde_json::json!({
                "session_id": session_id, "shell": shell_path, "cwd": workdir, "pid": pid,
            }));

            Ok(session_id)
        }
    }
}

pub fn write_to_terminal(
    registry: TerminalRegistry,
    session_id: &str,
    data: &[u8],
) -> Result<(), String> {
    let sessions = registry.lock().unwrap();
    let s = sessions.get(session_id)
        .ok_or_else(|| format!("Terminal session not found: {session_id}"))?;
    if s.exited { return Err("Terminal session has exited".into()); }
    let fd = s.master_fd.as_raw_fd();
    drop(sessions);
    nix::unistd::write(fd, data).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

pub fn resize_terminal(
    registry: TerminalRegistry,
    session_id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = registry.lock().unwrap();
    let s = sessions.get(session_id)
        .ok_or_else(|| format!("Terminal session not found: {session_id}"))?;
    if s.exited { return Err("Terminal session has exited".into()); }
    let fd = s.master_fd.as_raw_fd();
    let pid = s.pid;
    drop(sessions);

    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws); }
    let _ = signal::kill(Pid::from_raw(pid as i32), Signal::SIGWINCH);
    Ok(())
}

pub fn close_terminal(
    registry: TerminalRegistry,
    session_id: &str,
) -> Result<(), String> {
    let mut sessions = registry.lock().unwrap();
    let s = sessions.remove(session_id)
        .ok_or_else(|| format!("Terminal session not found: {session_id}"))?;
    let _ = signal::kill(Pid::from_raw(s.pid as i32), Signal::SIGTERM);
    drop(s);
    Ok(())
}
