use super::NativeBatchOptions;
use crate::error::{Error, Result};
use crate::native::Block;
use crate::native::NativeRow;
use crate::native::builder::BlockBuilder;
use crate::{NativeRow as DeriveNativeRow, Row};
use bytes::Bytes;
use std::sync::Arc;

#[derive(Row, DeriveNativeRow)]
#[clickhouse(crate = "crate")]
struct TestRow {
    value: Bytes,
}

fn row(size: usize) -> TestRow {
    TestRow {
        value: Bytes::from(vec![b'x'; size]),
    }
}

#[test]
fn validates_limits_and_preserves_defaults() {
    let defaults = NativeBatchOptions::default();
    assert_eq!(defaults.max_rows(), 8192);
    assert_eq!(defaults.max_estimated_bytes(), 4 * 1024 * 1024);
    for (rows, bytes) in [(0, 1), (1, 0), (0, 0)] {
        assert!(NativeBatchOptions::new(rows, bytes).is_err());
    }
    assert!(NativeBatchOptions::new(usize::MAX, usize::MAX).is_ok());
}

#[test]
fn bounds_rows_and_accepts_empty_input() {
    let options = NativeBatchOptions::new(2, usize::MAX).unwrap();
    let counts = options
        .blocks((0..5).map(|_| row(0)).collect())
        .map(|block| block.unwrap().num_rows())
        .collect::<Vec<_>>();
    assert_eq!(counts, [2, 2, 1]);
    assert_eq!(options.blocks::<TestRow>(vec![]).count(), 0);
}

#[test]
fn bounds_estimated_bytes_and_accepts_one_oversized_row() {
    let estimated = row(200).estimated_bytes();
    let options = NativeBatchOptions::new(10, estimated * 2).unwrap();
    let counts = options
        .blocks(vec![row(200), row(200), row(200), row(2048), row(200)])
        .map(|block| block.unwrap().num_rows())
        .collect::<Vec<_>>();
    assert_eq!(counts, [2, 1, 1, 1]);
}

#[test]
fn byte_limited_batches_do_not_reserve_the_row_limit() {
    #[derive(Row)]
    #[clickhouse(crate = "crate")]
    struct WideRow {
        value: [u64; 128],
    }

    impl NativeRow for WideRow {
        fn block(rows: Vec<Self>) -> Result<Block> {
            assert_eq!(rows.len(), 1);
            assert!(rows.capacity() <= 4, "capacity: {}", rows.capacity());
            let mut builder = BlockBuilder::new();
            builder
                .upsert_column("value")
                .unwrap()
                .add_all(rows.iter().map(|row| row.value[0]))
                .unwrap();
            builder.build().map_err(|error| Error::Other(error.into()))
        }

        fn estimated_bytes(&self) -> usize {
            size_of::<Self>()
        }
    }

    let options = NativeBatchOptions::new(1_048_576, 1).unwrap();
    let rows = (0..64).map(|_| WideRow { value: [7; 128] }).collect();
    let counts = options
        .blocks(rows)
        .map(|block| block.unwrap().num_rows())
        .collect::<Vec<_>>();
    assert_eq!(counts, vec![1; 64]);
}

#[test]
fn releases_completed_payloads_without_copying_or_building_ahead() {
    let payload = Arc::<[u8]>::from(vec![b'x'; 1024]);
    let next_payload = Arc::<[u8]>::from(vec![b'y'; 1024]);
    let options = NativeBatchOptions::new(1, 1).unwrap();
    let mut blocks = options.blocks(vec![
        TestRow {
            value: Bytes::from_owner(payload.clone()),
        },
        TestRow {
            value: Bytes::from_owner(next_payload.clone()),
        },
    ]);
    let block = blocks.next().unwrap().unwrap();
    assert_eq!(block.num_rows(), 1);
    assert_eq!(
        block["value"]
            .iter::<&str>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .as_ptr(),
        payload.as_ptr()
    );
    assert_eq!(Arc::strong_count(&payload), 2);
    assert_eq!(Arc::strong_count(&next_payload), 2);
    drop(block);
    assert_eq!(Arc::strong_count(&payload), 1);
    drop(blocks);
    assert_eq!(Arc::strong_count(&next_payload), 1);
}
