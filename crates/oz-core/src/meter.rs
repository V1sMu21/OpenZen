use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

static TOOL_CALLS: AtomicU64 = AtomicU64::new(0);
static LLM_TOKENS_IN: AtomicU64 = AtomicU64::new(0);
static LLM_TOKENS_OUT: AtomicU64 = AtomicU64::new(0);
static SESSIONS: AtomicU64 = AtomicU64::new(0);
static START: LazyLock<Instant> = LazyLock::new(Instant::now);

#[derive(Debug, Clone, serde::Serialize)]
pub struct MeterSnapshot {
    pub uptime_secs: f64,
    pub tool_calls: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub sessions: u64,
    pub tool_calls_per_min: f64,
    pub memory_dir_size: u64,
}

pub fn record_tool_call() {
    TOOL_CALLS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_tokens(in_tokens: u64, out_tokens: u64) {
    LLM_TOKENS_IN.fetch_add(in_tokens, Ordering::Relaxed);
    LLM_TOKENS_OUT.fetch_add(out_tokens, Ordering::Relaxed);
}

pub fn record_session() {
    SESSIONS.fetch_add(1, Ordering::Relaxed);
}

pub fn snapshot(memory_dir: Option<&std::path::Path>) -> MeterSnapshot {
    let uptime = START.elapsed().as_secs_f64();
    let calls = TOOL_CALLS.load(Ordering::Relaxed);
    let cpm = if uptime > 0.0 {
        calls as f64 / uptime * 60.0
    } else {
        0.0
    };

    let mem_size = memory_dir.map(compute_dir_size).unwrap_or(0);

    MeterSnapshot {
        uptime_secs: uptime,
        tool_calls: calls,
        tokens_in: LLM_TOKENS_IN.load(Ordering::Relaxed),
        tokens_out: LLM_TOKENS_OUT.load(Ordering::Relaxed),
        sessions: SESSIONS.load(Ordering::Relaxed),
        tool_calls_per_min: cpm,
        memory_dir_size: mem_size,
    }
}

fn compute_dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok(meta) = std::fs::metadata(&path) {
                    total += meta.len();
                }
            } else if path.is_dir() {
                total += compute_dir_size(&path);
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_defaults() {
        let snap = snapshot(None);
        assert!(snap.uptime_secs >= 0.0);
    }

    #[test]
    fn test_record_tool_call_increments() {
        let before = TOOL_CALLS.load(Ordering::Relaxed);
        record_tool_call();
        assert_eq!(TOOL_CALLS.load(Ordering::Relaxed), before + 1);
    }

    #[test]
    fn test_record_session_increments() {
        let before = SESSIONS.load(Ordering::Relaxed);
        record_session();
        assert_eq!(SESSIONS.load(Ordering::Relaxed), before + 1);
    }

    #[test]
    fn test_record_tokens() {
        record_tokens(100, 50);
        assert!(LLM_TOKENS_IN.load(Ordering::Relaxed) >= 100);
        assert!(LLM_TOKENS_OUT.load(Ordering::Relaxed) >= 50);
    }
}
