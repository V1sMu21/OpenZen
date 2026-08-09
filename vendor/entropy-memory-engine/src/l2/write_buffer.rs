use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::core::{MemoryInput, MemoryResult};

/// Configuration for the write buffer.
#[derive(Debug, Clone)]
pub struct WriteBufferConfig {
    /// Number of entries before forced flush.
    pub capacity: usize,
    /// Auto-flush interval in milliseconds (background thread).
    pub flush_interval_ms: u64,
    /// Enable background auto-flush thread.
    pub auto_flush_enabled: bool,
}

impl Default for WriteBufferConfig {
    fn default() -> Self {
        Self {
            capacity: 100,
            flush_interval_ms: 5_000,
            auto_flush_enabled: true,
        }
    }
}

/// A batch-collection write buffer.
///
/// Writes are buffered and flushed either when:
/// - The buffer reaches `capacity` entries (forced flush)
/// - The `flush_interval_ms` timer expires (timeout flush)
/// - `flush()` is called explicitly
///
/// A background thread handles timer-based flushes when
/// `auto_flush_enabled` is true (default).
pub struct WriteBuffer {
    config: WriteBufferConfig,
    buffer: Mutex<Vec<MemoryInput>>,
    last_flush: Mutex<Instant>,
    /// Signal the background thread to stop.
    stop_signal: Arc<AtomicBool>,
    /// Background thread handle (set by start_background_flush).
    #[allow(dead_code)]
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl WriteBuffer {
    pub fn new(config: WriteBufferConfig) -> Self {
        Self {
            buffer: Mutex::new(Vec::with_capacity(config.capacity)),
            last_flush: Mutex::new(Instant::now()),
            stop_signal: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
            config,
        }
    }

    pub fn len(&self) -> usize {
        self.buffer.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn config(&self) -> &WriteBufferConfig {
        &self.config
    }

    /// Add an entry to the buffer. If the buffer is full or the flush
    /// interval has elapsed, returns the batch for the caller to process.
    pub fn add(&self, input: MemoryInput) -> MemoryResult<Option<Vec<MemoryInput>>> {
        let mut buffer = self.buffer.lock();
        buffer.push(input);

        if buffer.len() >= self.config.capacity {
            let batch = buffer.drain(..).collect();
            *self.last_flush.lock() = Instant::now();
            return Ok(Some(batch));
        }

        let elapsed = self.last_flush.lock().elapsed();
        if elapsed >= Duration::from_millis(self.config.flush_interval_ms) && !buffer.is_empty() {
            let batch = buffer.drain(..).collect();
            *self.last_flush.lock() = Instant::now();
            return Ok(Some(batch));
        }

        Ok(None)
    }

    /// Drain all buffered entries.
    pub fn flush(&self) -> Vec<MemoryInput> {
        let mut buffer = self.buffer.lock();
        let batch = buffer.drain(..).collect();
        *self.last_flush.lock() = Instant::now();
        batch
    }

    /// Discard buffered entries without flushing.
    pub fn discard(&self) {
        self.buffer.lock().clear();
    }

    /// Start a background thread that calls `flush()` every
    /// `flush_interval_ms`. The thread runs as a daemon so it
    /// does not prevent process exit. Call `stop()` to terminate.
    ///
    /// This is a hard-real-time replacement for the old tokio-based
    /// auto-flush which silently failed when no tokio runtime was active.
    pub fn start_background_flush(self: &Arc<Self>) {
        if !self.config.auto_flush_enabled {
            return;
        }
        let interval = self.config.flush_interval_ms.max(100); // minimum 100ms
        let stop = self.stop_signal.clone();
        let wb = Arc::clone(self);

        let handle = std::thread::Builder::new()
            .name("write-buffer-flush".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(interval));
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let batch = wb.flush();
                if !batch.is_empty() {
                    tracing::trace!("WriteBuffer: background flushed {} entries", batch.len());
                }
            })
            .expect("failed to spawn WriteBuffer background thread");

        *self.handle.lock() = Some(handle);
    }

    /// Signal the background thread to stop. Blocks until the thread exits.
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.lock().take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WriteBuffer {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Fact, MemoryContent};

    fn make_input(i: u32) -> MemoryInput {
        MemoryInput::new(MemoryContent::Fact(Fact::new("k", "v", i.to_string())))
    }

    #[test]
    fn test_new_buffer_empty() {
        let buf = WriteBuffer::new(WriteBufferConfig {
            auto_flush_enabled: false,
            ..Default::default()
        });
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_add_no_flush() {
        let buf = WriteBuffer::new(WriteBufferConfig {
            capacity: 10,
            flush_interval_ms: 60_000,
            auto_flush_enabled: false,
        });
        for i in 0..5 {
            let result = buf.add(make_input(i)).unwrap();
            assert!(result.is_none());
        }
        assert_eq!(buf.len(), 5);
    }

    #[test]
    fn test_add_triggers_flush_on_full() {
        let buf = WriteBuffer::new(WriteBufferConfig {
            capacity: 3,
            flush_interval_ms: 60_000,
            auto_flush_enabled: false,
        });
        assert!(buf.add(make_input(1)).unwrap().is_none());
        assert!(buf.add(make_input(2)).unwrap().is_none());
        let batch = buf.add(make_input(3)).unwrap();
        assert!(batch.is_some());
        assert_eq!(batch.unwrap().len(), 3);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_flush_returns_all() {
        let buf = WriteBuffer::new(WriteBufferConfig {
            capacity: 100,
            flush_interval_ms: 60_000,
            auto_flush_enabled: false,
        });
        buf.add(make_input(1)).unwrap();
        buf.add(make_input(2)).unwrap();
        let batch = buf.flush();
        assert_eq!(batch.len(), 2);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_discard() {
        let buf = WriteBuffer::new(WriteBufferConfig {
            auto_flush_enabled: false,
            ..Default::default()
        });
        buf.add(make_input(1)).unwrap();
        buf.add(make_input(2)).unwrap();
        assert_eq!(buf.len(), 2);
        buf.discard();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_background_flush_runs_and_stops() {
        let buf = Arc::new(WriteBuffer::new(WriteBufferConfig {
            capacity: 100,
            flush_interval_ms: 100,
            auto_flush_enabled: true,
        }));

        buf.start_background_flush();
        // Add some entries — they should be flushed by the background thread
        buf.add(make_input(1)).unwrap();
        buf.add(make_input(2)).unwrap();

        // Give the thread time to flush
        std::thread::sleep(Duration::from_millis(300));

        // Buffer should be empty (flushed by background thread)
        assert!(buf.is_empty());

        buf.stop();
    }

    #[test]
    fn test_background_flush_disabled() {
        let buf = Arc::new(WriteBuffer::new(WriteBufferConfig {
            capacity: 100,
            flush_interval_ms: 100,
            auto_flush_enabled: false,
        }));

        buf.start_background_flush(); // should be no-op
        buf.add(make_input(1)).unwrap();

        std::thread::sleep(Duration::from_millis(200));
        // Buffer should NOT be empty since auto-flush is disabled
        assert_eq!(buf.len(), 1);
    }
}
