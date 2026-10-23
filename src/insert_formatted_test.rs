use super::{InsertFormatted, InsertState};
use crate::error::Error;
use crate::native::DataTypeNode;
use crate::native::builder::BlockBuilder;
use crate::native::writer::BlockWriter;
use crate::request_body::RequestBody;
use crate::{Client, Compression};
use bytes::Bytes;
use futures_util::poll;
use http_body_util::BodyExt;
use tokio::sync::oneshot;

#[tokio::test]
async fn cancelling_a_partial_compressed_native_block_aborts_the_body() {
    let compressions = [
        Compression::Lz4,
        #[cfg(feature = "zstd")]
        Compression::zstd(),
    ];
    for compression in compressions {
        let client = Client::default().with_compression(compression);
        let (sender, mut body) = RequestBody::chunked();
        let (completed, completion) = oneshot::channel();
        let mut insert = InsertFormatted::new(&client, "INSERT FORMAT Native".into(), None);
        insert.state = InsertState::Active {
            sender,
            handle: tokio::spawn(async { completion.await.unwrap() }),
            sent_bytes: 0,
            encoded_bytes: 0,
        };
        let mut writer = BlockWriter::new(insert);
        let mut builder = BlockBuilder::new();
        builder
            .upsert_bytes_column("value", DataTypeNode::String)
            .unwrap()
            .add(Bytes::from(vec![7; 3 * 1024 * 1024]));
        let block = builder.build().unwrap();
        let mut write = Box::pin(writer.write(&block));

        assert!(poll!(&mut write).is_pending());
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(
            u32::from_le_bytes(first[21..25].try_into().unwrap()),
            1024 * 1024
        );
        assert!(poll!(&mut write).is_pending());
        drop(write);

        let second = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(
            u32::from_le_bytes(second[21..25].try_into().unwrap()),
            1024 * 1024
        );
        let error = body.frame().await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "aborted");
        assert!(body.frame().await.is_none());
        assert!(poll!(Box::pin(writer.write(&block))).is_pending());
        completed
            .send(Err(Error::Other("aborted request body".into())))
            .unwrap();
        assert!(writer.end().await.is_err());
    }
}
