use bytes::Bytes;
use clickhouse::native::row::NativeBatchOptions;
use clickhouse::serde::RawBinary;
use clickhouse::{Client, Compression, NativeRow, Row, RowWrite, error::Result};
use cpu_time::ProcessTime;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use http_body_util::Full;
use hyper::Response;
use serde::Serialize;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

mod common;

#[global_allocator]
static ALLOCATOR: dhat::Alloc = dhat::Alloc;

const ADDR: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 6525));

#[derive(Row, NativeRow, Serialize)]
struct ScalarRow {
    timestamp: u64,
    count: u64,
    total: f64,
    minimum: f64,
    maximum: f64,
}

#[derive(Row, NativeRow, Serialize)]
struct DimensionRow {
    timestamp: u64,
    name: String,
    #[native(map)]
    dimensions: Vec<(String, String)>,
    count: u64,
}

#[derive(Row, NativeRow, Serialize)]
struct PayloadRow {
    timestamp: u64,
    payload: Bytes,
}

#[derive(Row, NativeRow, Serialize)]
struct AggregateRow {
    timestamp: u64,
    #[native(aggregate = "uniqCombined(14)", argument = "String")]
    state: RawBinary,
}

fn scalar_rows() -> Vec<ScalarRow> {
    (0..4096)
        .map(|timestamp| ScalarRow {
            timestamp,
            count: timestamp % 19,
            total: timestamp as f64 * 1.25,
            minimum: 1.0,
            maximum: 32.0,
        })
        .collect()
}

fn dimension_rows() -> Vec<DimensionRow> {
    (0..2048)
        .map(|timestamp| DimensionRow {
            timestamp,
            name: "network.request.duration".into(),
            dimensions: ["platform", "version", "route", "region", "status", "device"]
                .into_iter()
                .map(|key| (key.into(), format!("value-{}", timestamp % 37)))
                .collect(),
            count: 1,
        })
        .collect()
}

fn payload(index: usize, size: usize) -> Vec<u8> {
    let mut random = rand::rngs::SmallRng::seed_from_u64(index as u64);
    let mut bytes = vec![0; size];
    rand::RngCore::fill_bytes(&mut random, &mut bytes);
    bytes
}

use rand::SeedableRng;

fn payload_rows() -> Vec<PayloadRow> {
    (0..128)
        .map(|index| PayloadRow {
            timestamp: index as u64,
            payload: payload(index, 64 * 1024).into(),
        })
        .collect()
}

fn aggregate_rows() -> Vec<AggregateRow> {
    (0..256)
        .map(|index| AggregateRow {
            timestamp: index as u64,
            state: RawBinary(payload(index, 16 * 1024)),
        })
        .collect()
}

async fn send_rows<T: NativeRow + RowWrite + for<'a> Row<Value<'a> = T>>(
    client: &Client,
    rows: Vec<T>,
    native: bool,
) -> Result<()> {
    if native {
        let mut insert = client.insert_native("table");
        for block in NativeBatchOptions::default().blocks(rows) {
            insert.write(&block?).await?;
        }
        insert.end().await
    } else {
        let mut insert = client.insert::<T>("table").await?;
        for row in &rows {
            insert.write(row).await?;
        }
        insert.end().await
    }
}

async fn run_case<T: NativeRow + RowWrite + for<'a> Row<Value<'a> = T>>(
    make_rows: fn() -> Vec<T>,
    name: &'static str,
    compression: Compression,
    native: bool,
    iterations: u64,
    profile: bool,
) -> Result<Duration> {
    let _server = common::start_server(ADDR, |request| async {
        common::skip_incoming(request).await;
        Response::new(Full::new(Bytes::new()))
    })
    .await;
    let client = Client::default()
        .with_url(format!("http://{ADDR}"))
        .with_validation(false)
        .with_compression(compression);
    send_rows(&client, make_rows(), native).await?;
    let cpu = ProcessTime::now();
    let start = Instant::now();
    for _ in 0..iterations {
        send_rows(&client, make_rows(), native).await?;
    }
    let elapsed = start.elapsed();
    let cpu_elapsed = cpu.elapsed();
    if profile {
        let profiler = dhat::Profiler::builder().testing().build();
        send_rows(&client, make_rows(), native).await?;
        let heap = dhat::HeapStats::get();
        println!(
            "{name},{compression:?},{native},{iterations},{},{},{},{},{}",
            elapsed.as_nanos(),
            cpu_elapsed.as_nanos(),
            heap.total_blocks,
            heap.total_bytes,
            heap.max_bytes,
        );
        drop(profiler);
    }
    Ok(elapsed)
}

fn compare<T: NativeRow + RowWrite + for<'a> Row<Value<'a> = T> + Send + Sync + 'static>(
    criterion: &mut Criterion,
    name: &'static str,
    make_rows: fn() -> Vec<T>,
) {
    let runner = common::start_runner();
    let profile = std::env::var_os("NATIVE_INSERT_PROFILE").is_some();
    let mut group = criterion.benchmark_group(format!("native_comparison/{name}"));
    group.throughput(Throughput::Elements(make_rows().len() as u64));
    for compression in [
        Compression::None,
        #[cfg(feature = "lz4")]
        Compression::Lz4,
        #[cfg(feature = "zstd")]
        Compression::zstd(),
    ] {
        for native in [false, true] {
            if profile {
                runner.run(run_case(make_rows, name, compression, native, 20, true));
            } else {
                group.bench_with_input(
                    BenchmarkId::new(
                        format!("{compression:?}"),
                        if native { "Native" } else { "RowBinary" },
                    ),
                    &native,
                    |bench, &native| {
                        bench.iter_custom(|iterations| {
                            runner.run(run_case(
                                make_rows,
                                name,
                                compression,
                                native,
                                iterations,
                                false,
                            ))
                        });
                    },
                );
            }
        }
    }
    group.finish();
}

fn comparison(criterion: &mut Criterion) {
    if std::env::var_os("NATIVE_INSERT_PROFILE").is_some() {
        println!(
            "workload,compression,native,batches,wall_ns,cpu_ns,allocations,allocated_bytes,peak_heap_bytes"
        );
    }
    compare(criterion, "scalars", scalar_rows);
    compare(criterion, "dimensions", dimension_rows);
    compare(criterion, "bytes", payload_rows);
    compare(criterion, "aggregate", aggregate_rows);
}

criterion_group!(benches, comparison);
criterion_main!(benches);
