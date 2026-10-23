use super::{ColumnCodec, NativeColumn, append_encoded};
use crate::error::{BoxedError, Result};
use crate::native::DataTypeNode;
use crate::native::builder::BlockBuilder;
use crate::native::encode::{Encode, ValueWriter};
#[cfg(feature = "time")]
use time::OffsetDateTime;

/// Use a field type's [`NativeColumn`] implementation.
pub struct DefaultColumn;

impl<Value: NativeColumn> ColumnCodec<Value> for DefaultColumn {
    fn append<'a>(
        builder: &mut BlockBuilder,
        name: &str,
        values: impl Iterator<Item = &'a mut Value>,
    ) -> Result<()>
    where
        Value: 'a,
    {
        Value::append(builder, name, values)
    }

    fn estimated_bytes(value: &Value) -> usize {
        value.estimated_bytes()
    }
}

/// Encode an ordered vector of key/value pairs as a Map, preserving duplicates and ordering.
pub struct Map;

struct MapValue<'a, Key, Value>(&'a [(Key, Value)]);

impl<Key: Encode, Value: Encode> Encode for MapValue<'_, Key, Value> {
    fn produces() -> DataTypeNode {
        DataTypeNode::Map([Box::new(Key::produces()), Box::new(Value::produces())])
    }

    fn encode(&self, writer: &mut ValueWriter<'_>) -> std::result::Result<(), BoxedError> {
        let mut map = writer.write_map()?;
        for (key, value) in self.0 {
            map.write(key, value)?;
        }
        map.finish();
        Ok(())
    }
}

impl<Key: NativeColumn, Value: NativeColumn> ColumnCodec<Vec<(Key, Value)>> for Map {
    fn append<'a>(
        builder: &mut BlockBuilder,
        name: &str,
        values: impl Iterator<Item = &'a mut Vec<(Key, Value)>>,
    ) -> Result<()>
    where
        Key: 'a,
        Value: 'a,
    {
        append_encoded(
            builder,
            name,
            values.map(|value| MapValue(value.as_slice())),
        )
    }

    fn estimated_bytes(value: &Vec<(Key, Value)>) -> usize {
        value.iter().fold(8usize, |total, (key, value)| {
            total
                .saturating_add(key.estimated_bytes())
                .saturating_add(value.estimated_bytes())
        })
    }
}

/// Encode an OffsetDateTime as ClickHouse DateTime, validating unsigned second range.
#[cfg(feature = "time")]
pub struct DateTimeSeconds;

#[cfg(feature = "time")]
struct DateTimeValue(OffsetDateTime);

#[cfg(feature = "time")]
impl Encode for DateTimeValue {
    fn produces() -> DataTypeNode {
        DataTypeNode::DateTime(None)
    }

    fn encode(&self, writer: &mut ValueWriter<'_>) -> std::result::Result<(), BoxedError> {
        let seconds = u32::try_from(self.0.unix_timestamp())?;
        Ok(writer.write_fixed(&seconds.to_le_bytes())?)
    }
}

#[cfg(feature = "time")]
impl ColumnCodec<OffsetDateTime> for DateTimeSeconds {
    fn append<'a>(
        builder: &mut BlockBuilder,
        name: &str,
        values: impl Iterator<Item = &'a mut OffsetDateTime>,
    ) -> Result<()> {
        append_encoded(builder, name, values.map(|value| DateTimeValue(*value)))
    }

    fn estimated_bytes(value: &OffsetDateTime) -> usize {
        let _ = value;
        4
    }
}
