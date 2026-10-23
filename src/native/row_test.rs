use super::NativeRow;
use crate::native::LayoutKind;
use crate::serde::RawBinary;
use crate::{NativeRow as DeriveNativeRow, Row};
use bytes::Bytes;
use std::collections::HashMap;

#[derive(Row, DeriveNativeRow)]
#[clickhouse(crate = "crate")]
#[serde(rename_all = "camelCase")]
struct OwnedRow {
    binary_value: Bytes,
    label: String,
    #[native(map)]
    fields: Vec<(Bytes, Bytes)>,
    #[native(aggregate = "uniqCombined(14)", argument = "String")]
    unique: RawBinary,
    values: Vec<u16>,
    optional: Option<u64>,
    #[serde(skip)]
    ignored: String,
}

#[test]
fn derived_columns_preserve_owned_payloads_schema_and_values() {
    let binary_value = Bytes::from(vec![b'x'; 1024]);
    let binary_ptr = binary_value.as_ptr();
    let label = String::from("owned label");
    let label_ptr = label.as_ptr();
    let state = vec![0, 255, 1, 0, 2];
    let state_ptr = state.as_ptr();
    let row = OwnedRow {
        binary_value,
        label,
        fields: vec![
            (Bytes::from_static(b"z"), Bytes::from_static(b"1")),
            (Bytes::from_static(b"a"), Bytes::from_static(b"2")),
            (Bytes::from_static(b"z"), Bytes::from_static(b"3")),
        ],
        unique: RawBinary(state),
        values: vec![0, u16::MAX],
        optional: Some(u64::MAX),
        ignored: String::from("not a column"),
    };
    assert_eq!(row.ignored, "not a column");
    assert!(row.estimated_bytes() > 1024);
    let block = OwnedRow::block(vec![row]).unwrap();
    assert_eq!(block.num_rows(), 1);
    assert_eq!(
        block
            .columns()
            .iter()
            .map(|column| column.name())
            .collect::<Vec<_>>(),
        OwnedRow::COLUMN_NAMES
    );
    assert_eq!(
        block["binaryValue"]
            .iter::<&str>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .as_ptr(),
        binary_ptr
    );
    assert_eq!(
        block["label"]
            .iter::<&str>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .as_ptr(),
        label_ptr
    );
    let LayoutKind::Chunked { chunks } = &block["unique"].layout.kind else {
        panic!("state payload was copied")
    };
    assert_eq!(chunks[0].as_ptr(), state_ptr);
    assert_eq!(chunks[0].as_ref(), [0, 255, 1, 0, 2]);
    assert_eq!(
        block["unique"].data_type.to_string(),
        "AggregateFunction(uniqCombined(14), String)"
    );
    let LayoutKind::Map {
        end_indices,
        key_val_layouts,
    } = &block["fields"].layout.kind
    else {
        panic!("expected Map layout")
    };
    assert_eq!(end_indices.as_ref(), [3]);
    for (layout, expected) in key_val_layouts.iter().zip([b"zaz", b"123"]) {
        let LayoutKind::Variable { data, .. } = &layout.kind else {
            panic!("expected String layout")
        };
        assert_eq!(data.as_ref(), expected);
    }
    assert_eq!(
        block["fields"]
            .iter::<HashMap<String, String>>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap(),
        HashMap::from([
            (String::from("z"), String::from("3")),
            (String::from("a"), String::from("2"))
        ])
    );
    assert_eq!(
        block["values"]
            .iter::<Vec<u16>>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap(),
        [0, u16::MAX]
    );
    assert_eq!(
        block["optional"]
            .iter::<Option<u64>>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap(),
        Some(u64::MAX)
    );
    assert!(OwnedRow::block(vec![]).is_err());
}

#[derive(Row, DeriveNativeRow)]
#[clickhouse(crate = "crate")]
struct GenericRow<Value> {
    #[serde(rename = "renamed")]
    value: Value,
}

#[test]
fn derives_generic_rows_and_crate_aliases() {
    let block = GenericRow::block(vec![GenericRow { value: 7u32 }]).unwrap();
    let tuples = vec![(7u8, String::from("tuple"))];
    let array = GenericRow::block(vec![GenericRow {
        value: tuples.clone(),
    }])
    .unwrap();
    assert_eq!(
        array["renamed"].data_type.to_string(),
        "Array(Tuple(UInt8, String))"
    );
    assert_eq!(
        array["renamed"]
            .iter::<Vec<(u8, String)>>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap(),
        tuples
    );
    assert_eq!(
        block["renamed"]
            .iter::<u32>()
            .unwrap()
            .next()
            .unwrap()
            .unwrap(),
        7
    );
}

#[cfg(feature = "time")]
#[test]
fn derives_checked_datetime_codec() {
    use super::DateTimeSeconds;
    use time::OffsetDateTime;

    #[derive(Row, DeriveNativeRow)]
    #[clickhouse(crate = "crate")]
    struct TimestampRow {
        #[native(with = "DateTimeSeconds")]
        time: OffsetDateTime,
    }
    for (timestamp, valid) in [
        (0, true),
        (i64::from(u32::MAX), true),
        (-1, false),
        (i64::from(u32::MAX) + 1, false),
    ] {
        let row = TimestampRow {
            time: OffsetDateTime::from_unix_timestamp(timestamp).unwrap(),
        };
        assert_eq!(TimestampRow::block(vec![row]).is_ok(), valid);
    }
}
