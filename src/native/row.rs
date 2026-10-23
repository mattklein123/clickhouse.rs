//! Ownership-aware conversion of typed rows into Native columns.

#[cfg(test)]
#[path = "row_test.rs"]
mod tests;

mod batch;
mod codecs;

use crate::Row;
use crate::error::{Error, Result};
use crate::native::builder::BlockBuilder;
use crate::native::encode::Encode;
use crate::native::{Block, DataTypeNode};
use crate::serde::RawBinary;
use bytes::Bytes;
use std::error::Error as StdError;
use std::mem;

pub use batch::NativeBatchOptions;
#[cfg(feature = "time")]
pub use codecs::DateTimeSeconds;
pub use codecs::{DefaultColumn, Map};

/// A typed row that can transfer its payloads into a Native block.
///
/// Derive with [`crate::NativeRow`]. Column names and ordering come from [`Row`].
pub trait NativeRow: Row + Sized {
    /// Consume a nonempty batch, moving owned string and aggregate payloads into its columns.
    fn block(rows: Vec<Self>) -> Result<Block>;

    /// Estimate the row's serialized size for byte-bounded batching.
    fn estimated_bytes(&self) -> usize;
}

/// A column codec selected by `#[native(with = "path::Codec")]`.
pub trait ColumnCodec<Value> {
    /// Append one column from mutable row fields, taking ownership where appropriate.
    fn append<'a>(
        builder: &mut BlockBuilder,
        name: &str,
        values: impl Iterator<Item = &'a mut Value>,
    ) -> Result<()>
    where
        Value: 'a;

    /// Estimate the serialized field size without consuming it.
    fn estimated_bytes(value: &Value) -> usize;
}

/// Default Native column behavior for a Rust type.
///
/// Implement this alongside [`Encode`] for application wrappers. Override `append` to move
/// owned payloads; the default implementation uses the existing borrowed encoder.
pub trait NativeColumn: Encode + Sized {
    /// Append a column, using the borrowed encoder unless overridden for owned payloads.
    fn append<'a>(
        builder: &mut BlockBuilder,
        name: &str,
        values: impl Iterator<Item = &'a mut Self>,
    ) -> Result<()>
    where
        Self: 'a,
    {
        append_encoded(builder, name, values.map(|value| &*value))
    }

    /// Estimate the serialized field size, including variable-length framing.
    fn estimated_bytes(&self) -> usize;
}

fn boxed_error(error: impl StdError + Send + Sync + 'static) -> Error {
    Error::Other(Box::new(error))
}

fn append_encoded<Value: Encode>(
    builder: &mut BlockBuilder,
    name: &str,
    values: impl Iterator<Item = Value>,
) -> Result<()> {
    builder
        .upsert_column(name)
        .map_err(boxed_error)?
        .add_all(values)
        .map_err(Error::Other)?;
    Ok(())
}

macro_rules! scalar_columns {
    ($($ty:ty),* $(,)?) => {$ (
        impl NativeColumn for $ty {
            fn estimated_bytes(&self) -> usize {
                mem::size_of::<Self>()
            }
        }
    )*};
}

scalar_columns!(
    bool, u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64
);

impl NativeColumn for Bytes {
    fn append<'a>(
        builder: &mut BlockBuilder,
        name: &str,
        values: impl Iterator<Item = &'a mut Self>,
    ) -> Result<()> {
        builder
            .upsert_bytes_column(name, DataTypeNode::String)
            .map_err(boxed_error)?
            .add_all(values.map(mem::take));
        Ok(())
    }

    fn estimated_bytes(&self) -> usize {
        self.len().saturating_add(10)
    }
}

impl NativeColumn for String {
    fn append<'a>(
        builder: &mut BlockBuilder,
        name: &str,
        values: impl Iterator<Item = &'a mut Self>,
    ) -> Result<()> {
        builder
            .upsert_bytes_column(name, DataTypeNode::String)
            .map_err(boxed_error)?
            .add_all(values.map(|value| Bytes::from(mem::take(value))));
        Ok(())
    }

    fn estimated_bytes(&self) -> usize {
        self.len().saturating_add(10)
    }
}

macro_rules! tuple_columns {
    ($first_value:ident: $first:ident $(, $value:ident: $field:ident)* $(,)?) => {
        impl<$first: NativeColumn $(, $field: NativeColumn)*> NativeColumn for ($first, $($field,)*) {
            fn estimated_bytes(&self) -> usize {
                let ($first_value, $($value,)*) = self;
                $first_value.estimated_bytes()$(.saturating_add($value.estimated_bytes()))*
            }
        }
        tuple_columns!($($value: $field),*);
    };
    () => {};
}

tuple_columns!(
    field1: Field1, field2: Field2, field3: Field3, field4: Field4, field5: Field5,
    field6: Field6, field7: Field7, field8: Field8, field9: Field9, field10: Field10,
    field11: Field11, field12: Field12, field13: Field13, field14: Field14,
    field15: Field15, field16: Field16,
);

impl<Value: NativeColumn> NativeColumn for Vec<Value> {
    fn estimated_bytes(&self) -> usize {
        self.iter().fold(8usize, |total, value| {
            total.saturating_add(value.estimated_bytes())
        })
    }
}

impl<Value: NativeColumn> NativeColumn for Option<Value> {
    fn estimated_bytes(&self) -> usize {
        1usize.saturating_add(
            self.as_ref()
                .map_or(mem::size_of::<Value>(), NativeColumn::estimated_bytes),
        )
    }
}

/// Append opaque states for a single-argument aggregate function.
///
/// Used by the derive. Payloads must match the complete function, parameters, and argument type.
#[doc(hidden)]
pub fn append_aggregate<'a>(
    builder: &mut BlockBuilder,
    name: &str,
    function: &str,
    argument: &str,
    values: impl Iterator<Item = &'a mut RawBinary>,
) -> Result<()> {
    let argument = DataTypeNode::new(argument).map_err(boxed_error)?;
    builder
        .upsert_bytes_column(
            name,
            DataTypeNode::AggregateFunction(function.into(), vec![argument]),
        )
        .map_err(boxed_error)?
        .add_all(values.map(|value| Bytes::from(mem::take(&mut value.0))));
    Ok(())
}

/// Estimate the serialized size of an opaque aggregate state.
#[doc(hidden)]
pub fn aggregate_bytes(value: &RawBinary) -> usize {
    value.0.len()
}
