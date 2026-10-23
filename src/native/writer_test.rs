use super::BUFFER_SIZE;
use crate::error::BoxedError;
use crate::native::builder::BlockBuilder;
use crate::native::encode::{Encode, ValueWriter};
use crate::native::{DataTypeNode, varuint};
use crate::test::{Mock, handlers};
use crate::{Client, Compression};
use bytes::{BufMut, Bytes, BytesMut};

struct Binary<'a>(&'a [u8]);

impl Encode for Binary<'_> {
    fn produces() -> DataTypeNode {
        DataTypeNode::String
    }

    fn encode(&self, writer: &mut ValueWriter<'_>) -> Result<(), BoxedError> {
        Ok(writer.write_string(self.0)?)
    }
}

#[tokio::test]
async fn bounded_frames_preserve_native_blocks_and_raw_states() {
    #[cfg(not(feature = "zstd"))]
    let compressions = [Compression::None, Compression::Lz4];
    #[cfg(feature = "zstd")]
    let compressions = [Compression::None, Compression::Lz4, Compression::zstd()];
    for compression in compressions {
        for owned in [false, true] {
            verify_native_wire(compression, owned).await;
        }
    }
}

async fn verify_native_wire(compression: Compression, owned: bool) {
    let mock = Mock::new();
    let recorded = mock.add(handlers::record_request());
    let client = Client::default()
        .with_url(mock.real_url())
        .with_compression(compression);
    let strings = [vec![0u8; 128], vec![255u8; 2 * BUFFER_SIZE + 17], vec![]];
    let states = [
        1u64.to_le_bytes(),
        20u64.to_le_bytes(),
        300u64.to_le_bytes(),
    ];
    let mut builder = BlockBuilder::new();
    if owned {
        builder
            .upsert_bytes_column("value", DataTypeNode::String)
            .unwrap()
            .add_all(strings.iter().map(|value| Bytes::copy_from_slice(value)));
        builder
            .upsert_bytes_column(
                "state",
                DataTypeNode::AggregateFunction("sum".into(), vec![DataTypeNode::UInt64]),
            )
            .unwrap()
            .add_all(states.iter().map(|state| Bytes::copy_from_slice(state)));
    } else {
        builder
            .upsert_column("value")
            .unwrap()
            .add_all(strings.iter().map(|value| Binary(value)))
            .unwrap();
        builder
            .upsert_aggregate_column(
                "state",
                DataTypeNode::AggregateFunction("sum".into(), vec![DataTypeNode::UInt64]),
            )
            .unwrap()
            .add_all(states);
    }
    let block = builder.build().unwrap();
    let mut insert = client.insert_native("states");
    insert.write(&block).await.unwrap();
    insert.write(&block).await.unwrap();
    insert.end().await.unwrap();

    let request = recorded.request().await;
    let decompress = url::form_urlencoded::parse(request.uri().query().unwrap().as_bytes())
        .any(|(key, value)| key == "decompress" && value == "1");
    assert_eq!(decompress, compression != Compression::None);

    let mut expected = BytesMut::new();
    for _ in 0..2 {
        expected.put_u8(2);
        expected.put_u8(3);
        expected.extend_from_slice(b"\x05value\x06String");
        for string in &strings {
            varuint::write(&mut expected, string.len());
            expected.extend_from_slice(string);
        }
        let state_type = b"AggregateFunction(sum, UInt64)";
        expected.extend_from_slice(b"\x05state");
        expected.put_u8(state_type.len() as u8);
        expected.extend_from_slice(state_type);
        for state in states {
            expected.extend_from_slice(&state);
        }
    }

    if compression == Compression::None {
        assert_eq!(request.body().len(), expected.len());
        assert_eq!(
            request
                .body()
                .iter()
                .zip(&expected)
                .position(|(actual, expected)| actual != expected),
            None
        );
        return;
    }

    let mut body = &request.body()[..];
    let mut decoded = Vec::new();
    let mut frames = 0;
    while !body.is_empty() {
        let compressed_len = u32::from_le_bytes(body[17..21].try_into().unwrap()) as usize;
        let raw_len = u32::from_le_bytes(body[21..25].try_into().unwrap()) as usize;
        assert!(raw_len <= BUFFER_SIZE);
        let end = 16 + compressed_len;
        let checksum = cityhash_rs::cityhash_102_128(&body[16..end]).rotate_right(64);
        assert_eq!(&body[..16], checksum.to_le_bytes());
        match compression {
            Compression::Lz4 => {
                assert_eq!(body[16], 0x82);
                decoded.extend(lz4_flex::block::decompress(&body[25..end], raw_len).unwrap());
            }
            #[cfg(feature = "zstd")]
            Compression::Zstd(_) => {
                assert_eq!(body[16], 0x90);
                decoded.extend(zstd::bulk::decompress(&body[25..end], raw_len).unwrap());
            }
            _ => panic!("unexpected compression"),
        }
        body = &body[end..];
        frames += 1;
    }
    assert_eq!(frames, 6);
    assert_eq!(decoded.len(), expected.len());
    assert_eq!(
        decoded
            .iter()
            .zip(&expected)
            .position(|(actual, expected)| actual != expected),
        None
    );
    assert!(request.body().len() < expected.len() / 10);
}
