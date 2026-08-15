//! Command-line history persistence.
//!
//! Backed by a plain text file at `~/.openzen/history.txt` —
//! one line per submitted prompt, newest at the bottom. This is
//! the simple aichat / readline / bash approach; we don't need
//! reedline's SQLite-backed history for a chat agent CLI.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

const HISTORY_FILE: &str = "history.txt";
const HISTORY_LIMIT: usize = 1000;

pub struct History {
    path: PathBuf,
    lines: Vec<String>,
    cursor: Option<usize>,
    dirty: bool,
}

impl History {
    pub fn new(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let lines = match File::open(&path) {
            Ok(f) => BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter(|l| !l.trim().is_empty())
                .collect(),
            Err(_) => Vec::new(),
        };
        History {
            path,
            lines,
            cursor: None,
            dirty: false,
        }
    }

    /// Append a successfully submitted line to history.
    pub fn append(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.lines.last().map(String::as_str) == Some(line) {
            return;
        }
        self.lines.push(line.to_string());
        if self.lines.len() > HISTORY_LIMIT {
            let drop = self.lines.len() - HISTORY_LIMIT;
            self.lines.drain(..drop);
        }
        self.cursor = None;
        self.dirty = true;
    }

    /// Persist history to disk.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
        {
            for line in &self.lines {
                let _ = writeln!(f, "{}", line);
            }
        }
        self.dirty = false;
    }

    /// Look up the line at `n` steps back from the most recent
    /// entry. `n == 0` is the most recent line.
    pub fn lookup(&self, n: usize) -> Option<String> {
        if self.lines.is_empty() || n >= self.lines.len() {
            return None;
        }
        Some(self.lines[self.lines.len() - 1 - n].clone())
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn cursor_mut(&mut self) -> &mut Option<usize> {
        &mut self.cursor
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }
}

impl Default for History {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = PathBuf::from(home).join(".openzen").join(HISTORY_FILE);
        Self::new(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("ga-tui-history-{}.txt", uuid::Uuid::new_v4()))
    }

    #[test]
    fn empty_history_returns_none() {
        let h = History::new(temp_path());
        assert!(h.lookup(0).is_none());
        assert!(h.lookup(5).is_none());
        assert!(h.is_empty());
    }

    #[test]
    fn append_and_lookup() {
        let mut h = History::new(temp_path());
        h.append("first command");
        h.append("second command");
        h.append("third command");

        assert_eq!(h.len(), 3);
        assert_eq!(h.lookup(0), Some("third command".into()));
        assert_eq!(h.lookup(1), Some("second command".into()));
        assert_eq!(h.lookup(2), Some("first command".into()));
        assert_eq!(h.lookup(3), None);
    }

    #[test]
    fn empty_or_whitespace_lines_are_skipped() {
        let mut h = History::new(temp_path());
        h.append("");
        h.append("   ");
        h.append("\t\n");
        h.append("real line");
        assert_eq!(h.len(), 1);
        assert_eq!(h.lookup(0), Some("real line".into()));
    }

    #[test]
    fn history_persists_across_instances() {
        let path = temp_path();
        {
            let mut h1 = History::new(path.clone());
            h1.append("persistent line");
            h1.save();
        }
        {
            let h2 = History::new(path);
            assert_eq!(h2.lookup(0), Some("persistent line".into()));
        }
    }

    #[test]
    fn consecutive_duplicates_dedup() {
        let mut h = History::new(temp_path());
        h.append("same");
        h.append("same");
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn history_limit_truncates_oldest() {
        let mut h = History::new(temp_path());
        for i in 0..(HISTORY_LIMIT + 5) {
            h.append(&format!("line {i}"));
        }
        assert_eq!(h.len(), HISTORY_LIMIT);
        assert_eq!(h.lookup(0), Some(format!("line {}", HISTORY_LIMIT + 4)));
    }
}
