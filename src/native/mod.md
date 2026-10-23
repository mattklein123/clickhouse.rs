Support for reading and writing  ClickHouse's [Native columnar format].

# Type Mapping

This module implements conversions for many Rust types to/from their ClickHouse equivalents:

| Rust type                         | Clickhouse type           | Notes                                                    |
|-----------------------------------|---------------------------|----------------------------------------------------------|
| `bool`                            | `Bool`                    |                                                          |
| `u{N}` (`u8`, etc.)               | `UInt{N}` (`UInt8`, etc.) | `[u8]` and `Vec<u8>` map to `Array(UInt8)`, not `String` |
| `i{N}` (`i8`, etc.)               | `Int{N}` (`Int8`, etc.)   |                                                          |
| `f32`, `f64`                      | `Float32`, `Float64`      |                                                          |
| `String`                          | `String`, `FixedString`   | Decoding errors if the string is not valid UTF-8         |
| `&str`                            | `String`, `FixedString`   | See `String` above, zero-copy decode also supported      |
| `Option<T>`                       | `Nullable(T)`             | Some data types may not be nullable; see [`Nullable`].   |
| `[T]`, `Vec<T>`                   | `Array(T)`                |                                                          |
| `(T...)`                          | `Tuple(T...)`             | Tuples up to 16 elements supported                       |
| `HashMap<K, V>`, `BTreeMap<K, V>` | `Map(K, V)`               | See [`Map`] for restrictions on key types.               |
| `Ipv4Addr`                        | `IPv4`                    |                                                          |
| `Ipv6Addr`                        | `IPv6`                    |                                                          |
| `IpAddr`                          | `IPv4`, `IPv6`            | Supported for decode only.                               |

`LowCardinality` is supported for any data types where it is valid. It is handled internally.

## Typed Row Inserts

Derive `clickhouse::NativeRow` alongside `clickhouse::Row` to generate column builders directly
from a named-field struct. Names, renames, skips, and ordering use the existing Row metadata.
`NativeRow::block` consumes a nonempty batch and moves top-level String, Bytes, and aggregate
payloads into their columns without concatenating copies.

```rust
use bytes::Bytes;
use clickhouse::{NativeRow, Row};
use clickhouse::serde::RawBinary;

#[derive(Row, NativeRow)]
struct Counter {
	app_id: Bytes,
	value: u64,
	#[native(map)]
	fields: Vec<(Bytes, Bytes)>,
	#[native(aggregate = "uniqCombined(14)", argument = "String")]
	unique: RawBinary,
}
```

Numeric and boolean fields, owned strings, arrays, tuples, and nullable values use the default
[`row::NativeColumn`] implementations. An unannotated `Vec<(K, V)>` is not inferred to be a Map;
`#[native(map)]` preserves its ordering and duplicate keys. Aggregate attributes declare the
complete function, including parameters, and its single argument type; payload validity remains
the caller's responsibility.

Use `#[native(with = "path::Codec")]` for other representations. With the `time` feature,
[`row::DateTimeSeconds`] encodes `time::OffsetDateTime` as DateTime with checked unsigned seconds.
Serde adapters require an explicit Native codec rather than being interpreted automatically.
Application wrappers can implement [`row::NativeColumn`] alongside [`encode::Encode`], overriding
column append to transfer their owned payloads.

The derive also generates a serialized-size estimate for batching. Batch limits, iteration,
and transport remain separate from the row schema. Conditional fields, flattening, container-level
Serde serialization conversions, and asymmetric serialization/deserialization skips are rejected;
`#[serde(skip)]` is supported.

Use [`row::NativeBatchOptions`] to lazily convert an owned row vector into blocks. Its defaults
are 8,192 rows and 4 MiB of estimated serialized data: conservative starting points to amortize
block overhead while limiting per-block work, not benchmark-derived optima or protocol limits.
`NativeBatchOptions::new(max_rows, max_estimated_bytes)` accepts nonzero overrides. A single
oversized row is emitted on its own; the byte target is not a hard memory bound, and unconsumed
input rows remain owned by the iterator. Blocks are independent of compression-frame boundaries,
and writing multiple blocks to an insert still uses one HTTP request.

## Raw Aggregate States

[`builder::BlockBuilder::upsert_aggregate_column`] accepts already serialized ClickHouse
`AggregateFunction` states. Supply the complete function name, including parameters, and its
argument types. Each added byte slice is one state; the writer concatenates states without
String length prefixes or offsets on the wire.

The caller is responsible for the state encoding matching the declared function and ClickHouse
version. Payloads are not semantically validated. Nullable and nested aggregate-state columns
are not supported by this builder API.

Native inserts inherit the client's compression setting. Serialization is coalesced into bounded
1 MiB frames; a logical block may span several compression frames. Large contiguous payloads
bypass the staging buffer, while smaller fragments are coalesced before compression.

## Owned Byte Columns

[`builder::BlockBuilder::upsert_bytes_column`] accepts owned [`bytes::Bytes`] values for top-level
`String` and `AggregateFunction` columns. It retains each allocation instead of concatenating
the payloads into a column buffer. Use `Bytes::from(vec)` to transfer an existing `Vec<u8>`
without copying, then move it into the column with `add` or `add_all`.

Each value is one String or one complete aggregate state. Strings receive length prefixes on
the wire; aggregate states do not. The same aggregate-state encoding requirements above apply.
Borrowed appends remain supported, but copy their bytes into owned storage. Nullable and nested
columns are not supported by this owned-byte builder API.

For details on the ClickHouse equivalents, see <https://clickhouse.com/docs/reference/data-types>.

## Optional Features
Some third-party types are also supported as optional features:

| Rust type      | Clickhouse type | Required Feature(s) | Notes |
|----------------|-----------------|---------------------|-------|
| [`uuid::Uuid`] | `UUID`          | `uuid`              |       |

[Native columnar format]: https://clickhouse.com/docs/reference/formats/Native
[`Nullable`]: https://clickhouse.com/docs/reference/data-types/nullable
[`Map`]: https://clickhouse.com/docs/reference/data-types/map
