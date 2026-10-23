#[cfg(test)]
#[path = "batch_test.rs"]
mod tests;

use super::NativeRow;
use crate::error::{Error, Result};
use crate::native::Block;

//
// NativeBatchOptions
//

/// Limits for lazily converting owned rows into Native blocks.
///
/// The byte limit is an estimated serialized-size target, not a hard memory bound. A single
/// oversized row is emitted on its own. The iterator retains the unconsumed input rows, and
/// splitting blocks does not split an insert into separate HTTP requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeBatchOptions {
    max_rows: usize,
    max_estimated_bytes: usize,
}

impl Default for NativeBatchOptions {
    fn default() -> Self {
        // These are conservative starting points, not benchmark-derived optima or protocol limits.
        // 8,192 rows amortizes per-block overhead while bounding work for small rows; 4 MiB targets
        // bounded block-building work for large payloads. Blocks can span the writer's independent
        // 1 MiB compression frames. Tune both limits against workload throughput, CPU, and latency.
        Self {
            max_rows: 8192,
            max_estimated_bytes: 4 * 1024 * 1024,
        }
    }
}

impl NativeBatchOptions {
    /// Construct limits, rejecting zero values that would prevent batching from making progress.
    pub fn new(max_rows: usize, max_estimated_bytes: usize) -> Result<Self> {
        if max_rows == 0 || max_estimated_bytes == 0 {
            return Err(Error::Other("Native batch limits must be nonzero".into()));
        }
        Ok(Self {
            max_rows,
            max_estimated_bytes,
        })
    }

    /// Maximum number of rows in each block.
    pub fn max_rows(self) -> usize {
        self.max_rows
    }

    /// Estimated serialized-size target for each block, except individual oversized rows.
    pub fn max_estimated_bytes(self) -> usize {
        self.max_estimated_bytes
    }

    /// Consume rows lazily, moving their payloads into one block at a time.
    pub fn blocks<T: NativeRow>(self, rows: Vec<T>) -> impl Iterator<Item = Result<Block>> {
        let mut rows = rows.into_iter().peekable();
        std::iter::from_fn(move || {
            rows.peek()?;
            let mut batch = Vec::new();
            let mut bytes: usize = 0;
            while batch.len() < self.max_rows {
                let Some(row) = rows.peek() else {
                    break;
                };
                let next = bytes.saturating_add(row.estimated_bytes());
                if !batch.is_empty() && next > self.max_estimated_bytes {
                    break;
                }
                batch.push(rows.next().expect("peeked row exists"));
                bytes = next;
            }
            Some(T::block(batch))
        })
    }
}
