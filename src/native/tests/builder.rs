use crate::native::LayoutKind;
use crate::native::builder::{BlockBuilder, BlockBuilderError, UpsertColumnError};
use crate::native::encode::{Encode, ValueWriter};
use bytes::Bytes;
use clickhouse_types::DataTypeNode;
use std::collections::BTreeMap;

#[test]
fn aborted_nullable_array_restores_null_bitmap() {
    struct AbortedArray;

    impl Encode for AbortedArray {
        fn produces() -> DataTypeNode {
            Vec::<Option<u8>>::produces()
        }

        fn encode(
            &self,
            writer: &mut ValueWriter<'_>,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            writer.write_array()?.write(Some(42u8))?;
            Ok(())
        }
    }

    let mut builder = BlockBuilder::new();
    builder
        .upsert_column("x")
        .unwrap()
        .add(vec![Some(7u8)])
        .unwrap();
    builder
        .upsert_column("x")
        .unwrap()
        .add(AbortedArray)
        .unwrap();
    builder
        .upsert_column("x")
        .unwrap()
        .add(vec![None, Some(9u8)])
        .unwrap();
    let block = builder.build().unwrap();
    let values = block["x"]
        .iter::<Vec<Option<u8>>>()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(values, vec![vec![Some(7)], vec![None, Some(9)]]);
}

#[test]
fn owned_columns_retain_allocations_and_allow_borrowed_appends() {
    let value = Bytes::from(vec![b'x'; 1024]);
    let pointer = value.as_ptr();
    let mut builder = BlockBuilder::new();
    builder
        .upsert_column("value")
        .unwrap()
        .add("prefix")
        .unwrap();
    builder
        .upsert_bytes_column("value", DataTypeNode::String)
        .unwrap()
        .add(value);
    builder
        .upsert_column("value")
        .unwrap()
        .add("suffix")
        .unwrap();
    let data_type = DataTypeNode::AggregateFunction("sum".into(), vec![DataTypeNode::UInt64]);
    let state = Bytes::from(vec![1u8; 8]);
    let state_pointer = state.as_ptr();
    builder
        .upsert_bytes_column("state", data_type.clone())
        .unwrap()
        .add_all([state, Bytes::from_static(&[0; 8])]);
    builder
        .upsert_aggregate_column("state", data_type)
        .unwrap()
        .add(&[2; 8]);
    let block = builder.build().unwrap();
    assert_eq!(block.num_rows(), 3);
    let values = block["value"]
        .iter::<&str>()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(values[0], "prefix");
    assert_eq!(values[1].as_ptr(), pointer);
    assert_eq!(values[2], "suffix");
    let LayoutKind::Chunked { chunks } = &block["state"].layout.kind else {
        panic!("expected owned state storage");
    };
    assert_eq!(chunks[0].as_ptr(), state_pointer);
    assert!(matches!(
        BlockBuilder::new().upsert_bytes_column("x", DataTypeNode::UInt64),
        Err(UpsertColumnError::UnsupportedType { .. })
    ));
}

#[test]
fn aggregate_states_preserve_type_and_row_boundaries() {
    let data_type =
        DataTypeNode::AggregateFunction("uniqCombined(14)".into(), vec![DataTypeNode::String]);
    let mut builder = BlockBuilder::new();
    builder
        .upsert_column("id")
        .unwrap()
        .add_all([1u64, 2])
        .unwrap();
    let mut column = builder
        .upsert_aggregate_column("state", data_type.clone())
        .unwrap();
    column.add_all([&[0u8][..], &[1, 2, 3, 4, 5]]);
    assert_eq!(column.num_values(), 2);
    let block = builder.build().unwrap();
    assert_eq!(block.num_rows(), 2);
    assert_eq!(block["state"].data_type(), &data_type);
    let LayoutKind::Variable { end_offsets, data } = &block["state"].layout.kind else {
        panic!("expected variable-length state storage")
    };
    assert_eq!(&**end_offsets, &[1, 6]);
    assert_eq!(&data[..], &[0, 1, 2, 3, 4, 5]);
}

#[test]
fn aggregate_states_reject_wrong_types_and_lengths() {
    let mut builder = BlockBuilder::new();
    for data_type in [
        DataTypeNode::String,
        DataTypeNode::Nullable(Box::new(DataTypeNode::AggregateFunction(
            "sum".into(),
            vec![DataTypeNode::UInt64],
        ))),
        DataTypeNode::Array(Box::new(DataTypeNode::AggregateFunction(
            "sum".into(),
            vec![DataTypeNode::UInt64],
        ))),
    ] {
        assert!(matches!(
            builder.upsert_aggregate_column("state", data_type),
            Err(UpsertColumnError::UnsupportedType { .. })
        ));
    }
    builder
        .upsert_column("id")
        .unwrap()
        .add_all([1u64, 2])
        .unwrap();
    let data_type = DataTypeNode::AggregateFunction("sum".into(), vec![DataTypeNode::UInt64]);
    builder
        .upsert_aggregate_column("state", data_type.clone())
        .unwrap()
        .add(&1u64.to_le_bytes());
    assert!(matches!(
        builder.build(),
        Err(BlockBuilderError::MismatchedLengths { .. })
    ));
    assert!(matches!(
        builder.upsert_aggregate_column(
            "state",
            DataTypeNode::AggregateFunction("max".into(), vec![DataTypeNode::UInt64])
        ),
        Err(UpsertColumnError::ColumnExists { .. })
    ));
    builder
        .upsert_aggregate_column("state", data_type)
        .unwrap()
        .add(&2u64.to_le_bytes());
    assert_eq!(builder.build().unwrap().num_rows(), 2);
}

#[test]
fn forbids_incompatible_upsert() {
    let mut builder = BlockBuilder::new();

    builder.upsert_column::<i32>("foo").unwrap();

    let Err(e) = builder.upsert_column::<u32>("foo") else {
        panic!("expected error")
    };

    let UpsertColumnError::ColumnExists {
        name,
        existing_type,
        new_type,
    } = e
    else {
        panic!("unexpected error variant: {e:?}")
    };

    assert_eq!(*name, *"foo");
    assert_eq!(*existing_type, DataTypeNode::Int32);
    assert_eq!(*new_type, DataTypeNode::UInt32);
}

#[test]
fn forbids_mismatched_lengths() {
    let mut builder = BlockBuilder::new();

    builder
        .upsert_column::<i32>("foo")
        .unwrap()
        .add_all([0, 1, 2, 3, 4, 5])
        .unwrap();

    builder
        .upsert_column::<u32>("bar")
        .unwrap()
        .add_all([0, 1, 2, 3])
        .unwrap();

    let Err(err) = builder.build() else {
        panic!("expected error");
    };

    let BlockBuilderError::MismatchedLengths {
        longest_column,
        longest_len,
        shortest_column,
        shortest_len,
    } = err
    else {
        panic!("unexpected error variant: {err:?}")
    };

    assert_eq!(*longest_column, *"foo");
    assert_eq!(longest_len, 6);

    assert_eq!(*shortest_column, *"bar");
    assert_eq!(shortest_len, 4);
}

#[test]
fn debug() {
    let mut builder = BlockBuilder::new();

    assert_eq!(format!("{builder:?}"), "BlockBuilder { columns: [] }");

    builder
        .upsert_column::<i32>("foo")
        .unwrap()
        .add_all([0, 1, 2, 3, 4])
        .unwrap();

    // Saves us having to write these out by hand
    insta::assert_debug_snapshot!(builder);

    builder
        .upsert_column::<&str>("bar")
        .unwrap()
        .add_all(["lorem", "ipsum", "dolor", "sit", "amet"])
        .unwrap();

    insta::assert_debug_snapshot!(builder);

    builder
        .upsert_column::<&[u64]>("baz")
        .unwrap()
        .add_all([
            &[][..],
            &[0],
            &[0, 1],
            &[0, 1, 2],
            &[0, 1, 2, 3],
            &[0, 1, 2, 3, 4],
        ])
        .unwrap();

    insta::assert_debug_snapshot!(builder);

    builder
        .upsert_column("quux")
        .unwrap()
        .add_all((0u32..5).map(|i| (i, i.to_string())))
        .unwrap();

    insta::assert_debug_snapshot!(builder);

    builder
        .upsert_column("foobar")
        .unwrap()
        // `HashMap` order is not deterministic
        .add_all((0i64..5).map(|i| {
            (0..i)
                .map(|j| (j, j.to_string()))
                .collect::<BTreeMap<_, _>>()
        }))
        .unwrap();

    insta::assert_debug_snapshot!(builder);

    builder
        .upsert_column("foo_with_nulls")
        .unwrap()
        .add_all((0..5).map(|i| (i % 2 != 0).then_some(i)))
        .unwrap();

    insta::assert_debug_snapshot!(builder);
}

#[test]
fn errors_on_forbidden_types() {
    let mut builder = BlockBuilder::new();

    // https://clickhouse.com/docs/reference/data-types/nullable
    let Err(e) = builder.upsert_column::<Option<Vec<i32>>>("foo") else {
        panic!("expected error");
    };

    let UpsertColumnError::UnsupportedType {
        column_name,
        data_type,
    } = e
    else {
        panic!("unexpected error kind: {e:?}")
    };

    assert_eq!(*column_name, *"foo");
    assert_eq!(
        *data_type,
        DataTypeNode::Nullable(DataTypeNode::Array(DataTypeNode::Int32.into()).into())
    );

    let Err(e) = builder.upsert_column::<Option<BTreeMap<i32, String>>>("foo") else {
        panic!("expected error");
    };

    let UpsertColumnError::UnsupportedType {
        column_name,
        data_type,
    } = e
    else {
        panic!("unexpected error kind: {e:?}")
    };

    assert_eq!(*column_name, *"foo");
    assert_eq!(
        *data_type,
        DataTypeNode::Nullable(
            DataTypeNode::Map([DataTypeNode::Int32.into(), DataTypeNode::String.into(),]).into()
        )
    );
}

#[test]
fn errors_on_empty_block() {
    let mut builder = BlockBuilder::new();

    let Err(e) = builder.build() else {
        panic!("expected error");
    };

    assert!(
        matches!(e, BlockBuilderError::BlockEmpty),
        "unexpected error kind: {e:?}"
    );

    builder.upsert_column::<i32>("foo").unwrap();
    builder.upsert_column::<String>("bar").unwrap();

    let Err(e) = builder.build() else {
        panic!("expected error");
    };

    assert!(
        matches!(e, BlockBuilderError::BlockEmpty),
        "unexpected error kind: {e:?}"
    );
}
