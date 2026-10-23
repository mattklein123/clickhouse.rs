use super::{BlockReader, SAFE_ALLOCATION_LIMIT};
use crate::error::Error;
use crate::native::varuint;
use crate::response::Chunks;
use bytes::{BufMut, BytesMut};
use std::net::IpAddr;

fn reader(rows: usize, data_type: &str, payload: &[u8]) -> BlockReader {
    let mut bytes = BytesMut::new();
    varuint::write(&mut bytes, 1);
    varuint::write(&mut bytes, rows);
    bytes.extend_from_slice(b"\x01x");
    varuint::write(&mut bytes, data_type.len());
    bytes.extend_from_slice(data_type.as_bytes());
    bytes.extend_from_slice(payload);
    let mut reader = BlockReader::new(Chunks::empty());
    reader.inner.last_chunk = bytes.freeze();
    reader
}

#[tokio::test]
async fn rejects_decreasing_array_and_map_offsets() {
    let mut payload = BytesMut::new();
    payload.put_u64_le(2);
    payload.put_u64_le(1);
    payload.extend_from_slice(&[7, 8]);
    for data_type in ["Array(UInt8)", "Map(UInt8, UInt8)"] {
        let error = reader(2, data_type, &payload)
            .read_block()
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("invalid array offset"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn rejects_out_of_bounds_dictionary_keys() {
    for key_type in 0..4u64 {
        let mut payload = BytesMut::new();
        payload.put_u64_le(1);
        payload.put_u64_le(0x600 | key_type);
        payload.put_u64_le(1);
        payload.put_u8(7);
        payload.put_u64_le(1);
        payload.extend_from_slice(&2u64.to_le_bytes()[..1 << key_type]);
        let error = reader(1, "LowCardinality(UInt8)", &payload)
            .read_block()
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("dictionary key out of bounds"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn rejects_oversized_payload_before_allocating() {
    let data_type = format!("FixedString({})", usize::MAX);
    let error = reader(1, &data_type, &[0])
        .read_block()
        .await
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("safe allocation limit"),
        "{error}"
    );
    for data_type in ["String", "Array(UInt8)", "LowCardinality(UInt8)"] {
        let error = reader(SAFE_ALLOCATION_LIMIT, data_type, &[])
            .read_block()
            .await
            .err()
            .unwrap();
        assert!(matches!(error, Error::NotEnoughData));
    }
}

#[tokio::test]
async fn reads_nullable_simple_aggregate() {
    let block = reader(
        2,
        "SimpleAggregateFunction(anyLast, Nullable(UInt8))",
        &[0, 1, 7, 0],
    )
    .read_block()
    .await
    .unwrap()
    .unwrap();
    let values = block["x"]
        .iter::<Option<u8>>()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(values, vec![Some(7), None]);
}

#[tokio::test]
async fn decodes_simple_aggregate_and_low_cardinality_ip() {
    let address = 0x7f000001u32.to_le_bytes();
    let block = reader(1, "SimpleAggregateFunction(any, IPv4)", &address)
        .read_block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        block["x"]
            .iter::<IpAddr>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .to_string(),
        "127.0.0.1"
    );
    let mut payload = BytesMut::new();
    payload.put_u64_le(1);
    payload.put_u64_le(0x600);
    payload.put_u64_le(1);
    payload.extend_from_slice(&address);
    payload.put_u64_le(1);
    payload.put_u8(0);
    let block = reader(1, "LowCardinality(IPv4)", &payload)
        .read_block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        block["x"]
            .iter::<IpAddr>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .to_string(),
        "127.0.0.1"
    );
}
