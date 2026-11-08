//! Where lines are, in a big source, without storing every line.
//!
//! The source is cut into 64 KiB blocks and we remember how many newlines come before each block:
//! 8 bytes per 64 KiB, so a 300 GB file needs about 37 MB. "Which line is this byte on" and "where
//! does line N start" are then a lookup plus a scan of at most one block.
//!
//! Big files are indexed on a background thread while they're already open and scrollable; until
//! the scan gets somewhere, questions about that part answer `None`.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::RwLock;

pub const BLOCK: u64 = 64 << 10;

pub struct LineIndex {
    /// newlines before the start of block k
    before: RwLock<Vec<u64>>,
    scanned: AtomicU64,
    done: AtomicBool,
    cancel: AtomicBool,
    len: u64,
}

impl LineIndex {
    fn empty(len: u64) -> LineIndex {
        LineIndex {
            before: RwLock::new(vec![0]),
            scanned: AtomicU64::new(0),
            done: AtomicBool::new(len == 0),
            cancel: AtomicBool::new(false),
            len,
        }
    }

    /// Indexes `bytes` right now.
    pub fn build(bytes: &[u8]) -> LineIndex {
        let ix = LineIndex::empty(bytes.len() as u64);
        ix.scan(bytes);
        ix
    }

    /// An index filled in by a background thread. `bytes` is called on that thread to get the
    /// source's bytes (it keeps the source alive while it runs).
    pub fn background<S: std::ops::Deref<Target = [u8]> + Send + 'static>(len: u64, source: impl FnOnce() -> Option<S> + Send + 'static, ix: Arc<LineIndex>) {
        debug_assert_eq!(ix.len, len);
        std::thread::Builder::new()
            .name("margin-index".into())
            .spawn(move || {
                if let Some(bytes) = source() {
                    ix.scan(&bytes);
                }
            })
            .ok();
    }

    pub fn pending(len: u64) -> Arc<LineIndex> {
        Arc::new(LineIndex::empty(len))
    }

    fn scan(&self, bytes: &[u8]) {
        let len = bytes.len() as u64;
        let mut total = *self.before.read().last().unwrap_or(&0);
        let mut batch = Vec::with_capacity(256);
        let mut at = 0u64;
        while at < len {
            if self.cancel.load(Ordering::Relaxed) {
                return;
            }
            let end = (at + BLOCK).min(len);
            total += bytecount::count(&bytes[at as usize..end as usize], b'\n') as u64;
            at = end;
            batch.push(total);
            if batch.len() == 256 || at >= len {
                self.before.write().extend(batch.drain(..));
                self.scanned.store(at, Ordering::Release);
            }
        }
        self.scanned.store(len, Ordering::Release);
        self.done.store(true, Ordering::Release);
    }

    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn scanned(&self) -> u64 {
        self.scanned.load(Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Newlines in bytes[..x].
    fn before(&self, bytes: &[u8], x: u64) -> Option<u64> {
        let x = x.min(self.len);
        let k = (x / BLOCK) as usize;
        let base = {
            let b = self.before.read();
            *b.get(k)?
        };
        // knowing where the block starts is enough: the rest is counted on the spot
        let from = k as u64 * BLOCK;
        Some(base + bytecount::count(&bytes[from as usize..x as usize], b'\n') as u64)
    }

    /// Newlines in bytes[range].
    pub fn count(&self, bytes: &[u8], range: Range<u64>) -> Option<u64> {
        Some(self.before(bytes, range.end)? - self.before(bytes, range.start)?)
    }

    /// Position of the k'th (1-based) newline inside bytes[range].
    pub fn nth_newline(&self, bytes: &[u8], range: Range<u64>, k: u64) -> Option<u64> {
        let target = self.before(bytes, range.start)? + k; // its ordinal in the whole source
        let block = {
            let b = self.before.read();
            b.partition_point(|&n| n < target).saturating_sub(1)
        };
        let from = (block as u64 * BLOCK).max(range.start);
        let mut seen = self.before(bytes, from)?;
        for i in memchr::memchr_iter(b'\n', &bytes[from as usize..range.end as usize]) {
            seen += 1;
            if seen == target {
                return Some(from + i as u64);
            }
        }
        None
    }
}

impl Drop for LineIndex {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn across_blocks() {
        // lines of varying length spanning several blocks
        let mut v = Vec::new();
        let mut starts = vec![0u64];
        for i in 0..40_000u64 {
            v.extend(std::iter::repeat_n(b'x', (i % 17) as usize));
            v.push(b'\n');
            starts.push(v.len() as u64);
        }
        let ix = LineIndex::build(&v);
        assert!(ix.done());
        let n = v.len() as u64;
        assert_eq!(ix.count(&v, 0..n), Some(40_000));
        for &line in &[1u64, 2, 999, 12_345, 39_999, 40_000] {
            let pos = ix.nth_newline(&v, 0..n, line).unwrap();
            assert_eq!(pos + 1, starts[line as usize], "line {line}");
        }
        // a sub-range
        let a = starts[10_000];
        let b = starts[20_000];
        assert_eq!(ix.count(&v, a..b), Some(10_000));
        assert_eq!(ix.nth_newline(&v, a..b, 1).unwrap() + 1, starts[10_001]);
    }
}
