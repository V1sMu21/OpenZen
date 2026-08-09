use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};

use crate::core::{MemoryError, MemoryInput, MemoryResult};

const DEFAULT_WAL_CAPACITY: usize = 1_048_576;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WalEntryType {
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalEntry {
    pub seq: u64,
    pub entry_type: WalEntryType,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct WriteReceipt {
    pub seq: u64,
    pub id: u64,
}

#[derive(Debug, Clone)]
pub struct WalConfig {
    pub capacity: usize,
    pub flush_interval_ms: u64,
    pub auto_flush: bool,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_WAL_CAPACITY,
            flush_interval_ms: DEFAULT_FLUSH_INTERVAL_MS,
            auto_flush: false,
        }
    }
}

struct WalInner {
    buffer: RwLock<Vec<u8>>,
    is_running: AtomicBool,
}

pub struct Wal {
    config: WalConfig,
    inner: Arc<WalInner>,
    write_seq: AtomicU64,
    #[allow(dead_code)]
    read_seq: AtomicU64,
    entries_written: AtomicU64,
    flush_tx: mpsc::UnboundedSender<()>,
}

impl Wal {
    pub fn new(config: WalConfig) -> Self {
        let (flush_tx, mut flush_rx) = mpsc::unbounded_channel::<()>();
        let inner = Arc::new(WalInner {
            buffer: RwLock::new(Vec::with_capacity(config.capacity)),
            is_running: AtomicBool::new(true),
        });

        if config.auto_flush {
            let inner_clone = inner.clone();
            let flush_interval = Duration::from_millis(config.flush_interval_ms);

            tokio::spawn(async move {
                let mut interval = time::interval(flush_interval);
                loop {
                    tokio::select! {
                        _ = interval.tick() => {}
                        _ = flush_rx.recv() => {}
                    }
                    if !inner_clone.is_running.load(Ordering::Relaxed) {
                        break;
                    }
                }
            });
        }

        Self {
            inner,
            write_seq: AtomicU64::new(0),
            read_seq: AtomicU64::new(0),
            entries_written: AtomicU64::new(0),
            flush_tx,
            config,
        }
    }

    pub fn append(&self, input: &MemoryInput) -> MemoryResult<WriteReceipt> {
        let seq = self.write_seq.fetch_add(1, Ordering::SeqCst);
        let id = seq;

        let data = bincode_serialize(input)
            .map_err(|e| MemoryError::WalWrite(format!("serialization failed: {}", e)))?;

        let entry = WalEntry {
            seq,
            entry_type: WalEntryType::Insert,
            data,
        };

        let encoded = bincode_serialize(&entry)
            .map_err(|e| MemoryError::WalWrite(format!("entry encoding failed: {}", e)))?;

        {
            let mut buffer = self.inner.buffer.write();
            if buffer.len() + encoded.len() > self.config.capacity {
                buffer.clear();
            }
            buffer.extend_from_slice(&encoded);
        }

        self.entries_written.fetch_add(1, Ordering::Relaxed);

        let _ = self.flush_tx.send(());

        Ok(WriteReceipt { seq, id })
    }

    pub fn read_entries(&self) -> Vec<WalEntry> {
        let buffer = self.inner.buffer.read();
        if buffer.is_empty() {
            return Vec::new();
        }
        let mut entries = Vec::new();
        let mut offset = 0;
        while offset < buffer.len() {
            match bincode::deserialize::<WalEntry>(&buffer[offset..]) {
                Ok(entry) => {
                    // 精确计算序列化后占用的字节数
                    let serialized = bincode::serialize(&entry).unwrap_or_default();
                    let entry_size = serialized.len();
                    entries.push(entry);
                    offset += entry_size;
                }
                Err(_) => break,
            }
        }
        entries
    }

    pub fn total_entries(&self) -> u64 {
        self.entries_written.load(Ordering::Relaxed)
    }

    pub fn current_seq(&self) -> u64 {
        self.write_seq.load(Ordering::Relaxed)
    }

    pub fn flush(&self) {
        std::mem::drop(self.inner.buffer.write());
    }

    pub fn len(&self) -> usize {
        self.inner.buffer.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn shutdown(&self) {
        self.inner.is_running.store(false, Ordering::Relaxed);
    }
}

impl Drop for Wal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn bincode_serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, String> {
    bincode::serialize(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Fact;
    use crate::core::MemoryContent;

    #[tokio::test]
    async fn test_wal_new() {
        let config = WalConfig {
            auto_flush: false,
            ..Default::default()
        };
        let wal = Wal::new(config);
        assert!(wal.is_empty());
        assert_eq!(wal.total_entries(), 0);
        wal.shutdown();
    }

    #[tokio::test]
    async fn test_wal_append() {
        let config = WalConfig {
            auto_flush: false,
            ..Default::default()
        };
        let wal = Wal::new(config);
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("user", "likes", "rust")));
        let receipt = wal.append(&input).unwrap();
        assert_eq!(receipt.seq, 0);
        assert_eq!(wal.total_entries(), 1);
        assert_eq!(wal.current_seq(), 1);
        wal.shutdown();
    }

    #[tokio::test]
    async fn test_wal_multiple_entries() {
        let config = WalConfig {
            auto_flush: false,
            ..Default::default()
        };
        let wal = Wal::new(config);
        for i in 0..10 {
            let input = MemoryInput::new(MemoryContent::Fact(Fact::new(
                "key",
                "value",
                i.to_string(),
            )));
            wal.append(&input).unwrap();
        }
        assert_eq!(wal.total_entries(), 10);
        wal.shutdown();
    }

    #[tokio::test]
    async fn test_wal_receipt_ids() {
        let config = WalConfig {
            auto_flush: false,
            ..Default::default()
        };
        let wal = Wal::new(config);
        let r1 = wal
            .append(&MemoryInput::new(MemoryContent::Fact(Fact::new(
                "a", "b", "c",
            ))))
            .unwrap();
        let r2 = wal
            .append(&MemoryInput::new(MemoryContent::Fact(Fact::new(
                "d", "e", "f",
            ))))
            .unwrap();
        assert_eq!(r1.seq, 0);
        assert_eq!(r2.seq, 1);
        assert!(r2.seq > r1.seq);
        wal.shutdown();
    }

    #[tokio::test]
    async fn test_wal_flush() {
        let config = WalConfig {
            auto_flush: false,
            ..Default::default()
        };
        let wal = Wal::new(config);
        for _ in 0..5 {
            let input = MemoryInput::new(MemoryContent::Fact(Fact::new("x", "y", "z")));
            wal.append(&input).unwrap();
        }
        wal.flush();
        assert!(wal.len() > 0);
        wal.shutdown();
    }

    #[tokio::test]
    async fn test_wal_shutdown() {
        let config = WalConfig::default();
        let wal = Wal::new(config);
        wal.shutdown();
        assert!(!wal.inner.is_running.load(Ordering::Relaxed));
    }
}
