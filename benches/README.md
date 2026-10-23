# Benchmarks

All cases are run with `cargo bench --bench <case>`.

## With a mocked server

These benchmarks are run against a mocked server, which is a simple HTTP server that responds with a fixed response.
This is useful to measure the overhead of the client itself.

### Scenarios

* [mocked_select](mocked_select.rs) checks throughput of `Client::query()`.
* [mocked_insert](mocked_insert.rs) checks throughput of `Client::insert()` and `Client::inserter()`
  (requires `inserter` feature).
* [native_insert](native_insert.rs) compares RowBinary and Native inserts with scalar rows,
  dimension maps, 64 KiB byte payloads, and 16 KiB opaque aggregate payloads.

### Native versus RowBinary

```bash
cargo bench --bench native_insert --features zstd -- --sample-size 10
NATIVE_INSERT_PROFILE=1 cargo bench --bench native_insert --features zstd
```

Both paths send the same input in one HTTP request, with validation disabled and identical
compression settings. Native uses the default row/byte batch limits. Timings include input
creation, serialization, compression, and transport, but exclude server setup and the first
warm-up request. Criterion reports rows per second; these are client-overhead measurements,
not ClickHouse ingestion rates. The aggregate payload is synthetic opaque data, not a valid
aggregate state for insertion into a real server.

The profiling command prints CSV with wall/process CPU nanoseconds over 20 batches, followed
by allocation count, total allocated bytes, and peak tracked live heap for one separate batch.
CPU includes the client and mock-server threads. Heap tracking includes input creation and
client/server activity, excludes warm-up allocations, and is incremental tracked heap rather
than process RSS. Heap instrumentation is enabled only for that separate batch; its timings
are not included in the CPU/throughput measurement. DHAT testing mode writes no profile files.
Use multiple runs on an otherwise idle machine before drawing performance conclusions.

### How to collect perf data

The crate's code runs on the thread with the name `testee`:

```bash
cargo bench --bench <name> &
perf record -p `ps -AT | grep testee | awk '{print $2}'` --call-graph dwarf,65528 --freq 5000 -g -- sleep 5
perf script > perf.script
```

Then upload the `perf.script` file to [Firefox Profiler].

## With a running ClickHouse server

These benchmarks are run against a real ClickHouse server, so it must be started:

```bash
docker compose up -d
cargo bench --bench <case>
```

### Scenarios

* [select_numbers.rs](select_numbers.rs) measures time of running a big SELECT query to the `system.numbers_mt` table.
* [select_nyc_taxi_data.rs](select_nyc_taxi_data.rs) measures time of running a fairly large SELECT query (approximately
  3 million records) to the `nyc_taxi_data` table using the [NYC taxi dataset].

### How to collect perf data

```bash
cargo bench --bench <name> &
perf record -p `ps -AT | grep <name> | awk '{print $2}'` --call-graph dwarf,65528 --freq 5000 -g -- sleep 5
perf script > perf.script
```

Then upload the `perf.script` file to [Firefox Profiler].

<!-- links -->

[Firefox Profiler]: https://profiler.firefox.com

[NYC taxi dataset]: https://clickhouse.com/docs/getting-started/example-datasets/nyc-taxi#create-the-table-trips
