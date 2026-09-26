//! Manual probe against the real on-disk sources.
//! Run: `cargo run -p oz-import --example probe`

use oz_import::{ImportSource, SourcePaths};

fn main() {
    let paths = SourcePaths::default();
    println!("zcode db : {}", paths.zcode_db.display());
    println!("dsh dir  : {}", paths.dsh_sessions_dir.display());

    for status in oz_import::scan_sources(&paths) {
        println!(
            "\n[{}] available={} count={:?} detail={:?} error={:?}",
            status.id, status.available, status.session_count, status.detail, status.error
        );
    }

    for source in ImportSource::ALL {
        let started = std::time::Instant::now();
        let list = match oz_import::list_sessions(source, &paths) {
            Ok(l) => l,
            Err(e) => {
                println!("\n{}: list failed: {e}", source.id());
                continue;
            }
        };
        println!(
            "\n=== {} : {} sessions listed in {:?} ===",
            source.id(),
            list.len(),
            started.elapsed()
        );
        for s in list.iter().take(4) {
            println!(
                "  - {} | {} | {:?} | msgs={}",
                s.source_id, s.title, s.directory, s.message_count
            );
        }

        // Deep-parse the two largest sessions and validate the emitted shape.
        let mut probe: Vec<_> = list.iter().collect();
        probe.sort_by_key(|s| std::cmp::Reverse(s.message_count));
        let mut dumped = false;
        for target in probe.into_iter().take(2) {
            let t0 = std::time::Instant::now();
            match oz_import::read_session(source, &target.source_id, &paths) {
                Ok(session) => {
                    println!(
                        "\n  READ {} -> {} messages in {:?} (title={:?})",
                        target.source_id,
                        session.messages.len(),
                        t0.elapsed(),
                        session.title
                    );
                    validate(&session.messages);
                    // One sample per source, for cross-checking against the
                    // real frontend reducer (see the repo's verification notes).
                    if !dumped {
                        let out = format!("/tmp/oz-import-sample-{}.json", source.id());
                        if let Ok(json) = serde_json::to_string(&session.messages) {
                            let _ = std::fs::write(&out, json);
                            println!("    dumped -> {out}");
                        }
                        dumped = true;
                    }
                }
                Err(e) => println!("\n  READ {} failed: {e}", target.source_id),
            }
        }
    }
}

/// Mirror of the frontend's `parseSessionMessages` acceptance rules: a message
/// is dropped unless it has `content`, `tool_results` or `streamEvents`.
fn validate(messages: &[serde_json::Value]) {
    let mut dropped = 0;
    let mut with_events = 0;
    let mut roles = std::collections::BTreeMap::new();
    let mut event_types = std::collections::BTreeMap::new();
    let mut unfinished = 0;
    let mut tool_calls = 0;
    let mut tool_results = 0;

    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        *roles.entry(role.to_string()).or_insert(0) += 1;

        let has_content = m
            .get("content")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        let has_events = m
            .get("streamEvents")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        if !has_content && !has_events {
            dropped += 1;
        }
        if has_events {
            with_events += 1;
        }

        if role == "assistant" {
            let duration = m.get("duration").and_then(|v| v.as_i64()).unwrap_or(0);
            let exit = m.get("exitReason").and_then(|v| v.as_str()).unwrap_or("");
            if duration <= 0 && exit.is_empty() {
                unfinished += 1;
            }
        }

        for e in m
            .get("streamEvents")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let t = e.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            *event_types.entry(t.to_string()).or_insert(0) += 1;
            if t == "tool_input_available" {
                tool_calls += 1;
            }
            if t == "tool_output_available" {
                tool_results += 1;
            }
        }
    }

    println!("    roles           : {roles:?}");
    println!("    streamEvents ev : {event_types:?}");
    println!(
        "    dropped-by-frontend={dropped} withEvents={with_events} unfinishedAssistants={unfinished}"
    );
    println!("    toolInputAvailable={tool_calls} toolOutputAvailable={tool_results}");
    if dropped > 0 {
        println!("    !! {dropped} messages would be dropped by the frontend");
    }
    if unfinished > 0 {
        println!("    !! {unfinished} assistant turns would render as still-Running");
    }
    if let Some(first_user) = messages.iter().find(|m| m.get("role") == Some(&"user".into())) {
        println!("    first user msg  : {}", first_user);
    }
}
