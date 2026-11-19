//! Model test for the checkpoint WAL: append, reopen, torn tail, compaction.
//!
//! The oracle is a trivial list: the newest complete record must always be the
//! last one appended, no matter where the file was cut. This is the test that
//! would have caught the "resume replays a half-written checkpoint" class.

use std::path::Path;

use oz_core::checkpoint::{
    append_checkpoint_wal, read_latest_checkpoint_wal, LoopCheckpoint, MmapWal,
};

fn cp(turn: u32) -> LoopCheckpoint {
    LoopCheckpoint {
        turn,
        timestamp: turn as f64,
        messages: Vec::new(),
        history_info: Vec::new(),
        full_response: format!("turn {turn}"),
        exit_reason: None,
        session_id: Some("s1".into()),
        plan: Default::default(),
        todos: Vec::new(),
        interventions: Vec::new(),
        full_thinking: None,
        git_sha: None,
        git_branch: None,
        git_origin_url: None,
    }
}

#[test]
fn latest_is_the_last_complete_record_after_every_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let mut wal = MmapWal::open(dir.path(), "s1").unwrap();
    for turn in 1..=5 {
        wal.append(&cp(turn)).unwrap();
    }
    wal.flush().unwrap();
    let path = wal.path().to_path_buf();
    drop(wal);

    let full = std::fs::read(&path).unwrap();
    let records = record_ranges(&full);
    assert_eq!(records.len(), 5, "expected five length-prefixed records");

    // Cut inside the last record, at its start, and inside the 4th record.
    for (cut, want) in [
        (records[4].0 + 10, 4u32),
        (records[4].0, 4),
        (records[3].0 + 10, 3),
    ] {
        std::fs::write(&path, &full[..cut]).unwrap();
        let got = MmapWal::open(dir.path(), "s1").unwrap().latest();
        assert_eq!(
            got.map(|c| c.turn),
            Some(want),
            "cut at {cut} must fall back to the last complete record"
        );
    }

    // Cutting inside the header leaves nothing to recover — and still must
    // not panic.
    std::fs::write(&path, &full[..6]).unwrap();
    assert!(MmapWal::open(dir.path(), "s1").unwrap().latest().is_none());
}

/// [start, end) of each length-prefixed record after the 8-byte header.
fn record_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 8usize;
    while pos + 4 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        if len == 0 || pos + 4 + len > bytes.len() {
            break;
        }
        out.push((pos, pos + 4 + len));
        pos += 4 + len;
    }
    out
}

#[test]
fn reopening_returns_the_newest_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut wal = MmapWal::open(dir.path(), "s1").unwrap();
        for turn in 1..=3 {
            wal.append(&cp(turn)).unwrap();
        }
        wal.flush().unwrap();
    }
    let wal = MmapWal::open(dir.path(), "s1").unwrap();
    assert_eq!(wal.latest().map(|c| c.turn), Some(3));
    assert_eq!(wal.all().len(), 3);
}

#[test]
fn compaction_bounds_the_file_without_losing_the_newest_record() {
    let dir = tempfile::tempdir().unwrap();
    for turn in 1..=12 {
        append_checkpoint_wal(dir.path(), "s1", &cp(turn));
    }
    assert_eq!(
        read_latest_checkpoint_wal(dir.path(), "s1").map(|c| c.turn),
        Some(12)
    );
    // A fresh mmap bypasses the process-global WAL cache: this is the only
    // assertion that proves the *file* holds turn 12.
    let disk = MmapWal::open(dir.path(), "s1").unwrap();
    assert_eq!(disk.latest().map(|c| c.turn), Some(12));
    // Each record is a full snapshot; compaction keeps only the newest one.
    // Steady state is at most two records (the previous snapshot + the new
    // one); the point is that it does not grow with the number of appends.
    let records = MmapWal::open(dir.path(), "s1").unwrap().all().len();
    assert!(
        (1..=2).contains(&records),
        "compaction left {records} records (expected <= 2)"
    );
    let size = newest_wal_size(dir.path());
    assert!(size <= (1 << 21), "WAL capacity grew to {size} bytes");
}

fn newest_wal_size(dir: &Path) -> u64 {
    let mut best = 0;
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("mmap") {
            best = best.max(e.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    best
}
