//! Benchmarks for the JSONL parser and exporters.
//!
//! Run with: `cargo bench`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::io::BufReader;
use std::io::Cursor;

use claude_snatch::export::{ExportOptions, Exporter, JsonExporter, MarkdownExporter, TextExporter};
use claude_snatch::parser::JsonlParser;
use claude_snatch::reconstruction::Conversation;

/// Sample JSONL data for benchmarking.
fn generate_sample_jsonl(message_count: usize) -> String {
    let mut lines = Vec::with_capacity(message_count);

    for i in 0..message_count {
        let uuid = format!("{:08x}-0000-0000-0000-{:012x}", i, i);
        let parent_uuid = if i > 0 {
            format!(
                ",\"parentUuid\":\"{:08x}-0000-0000-0000-{:012x}\"",
                i - 1,
                i - 1
            )
        } else {
            String::new()
        };

        if i % 2 == 0 {
            // User message
            lines.push(format!(
                r#"{{"type":"user","uuid":"{}","sessionId":"test-session","version":"2.0.76","timestamp":"2025-01-01T00:00:{:02}Z","message":{{"role":"user","content":"Test message {}"}}{},"isSidechain":false}}"#,
                uuid, i % 60, i, parent_uuid
            ));
        } else {
            // Assistant message
            lines.push(format!(
                r#"{{"type":"assistant","uuid":"{}","sessionId":"test-session","version":"2.0.76","timestamp":"2025-01-01T00:00:{:02}Z","message":{{"role":"assistant","model":"claude-sonnet-4-20250514","content":[{{"type":"text","text":"Response {}"}}],"id":"msg_{}","stop_reason":"end_turn","usage":{{"input_tokens":100,"output_tokens":50}}}}{},"isSidechain":false}}"#,
                uuid, i % 60, i, i, parent_uuid
            ));
        }
    }

    lines.join("\n")
}

fn bench_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("parser");

    for size in [10, 100, 1000, 10000].iter() {
        let data = generate_sample_jsonl(*size);
        let bytes = data.len();

        group.throughput(Throughput::Bytes(bytes as u64));

        group.bench_with_input(BenchmarkId::new("parse_str", size), &data, |b, data| {
            b.iter(|| {
                let mut parser = JsonlParser::new();
                let entries = parser.parse_str(data);
                black_box(entries)
            });
        });

        group.bench_with_input(BenchmarkId::new("parse_reader", size), &data, |b, data| {
            b.iter(|| {
                let cursor = Cursor::new(data.as_bytes());
                let reader = BufReader::new(cursor);
                let mut parser = JsonlParser::new();
                let entries = parser.parse_reader(reader);
                black_box(entries)
            });
        });

        group.bench_with_input(
            BenchmarkId::new("parse_lenient", size),
            &data,
            |b, data| {
                b.iter(|| {
                    let mut parser = JsonlParser::new().with_lenient(true);
                    let entries = parser.parse_str(data);
                    black_box(entries)
                });
            },
        );
    }

    group.finish();
}

fn bench_reconstruction(c: &mut Criterion) {
    let mut group = c.benchmark_group("reconstruction");

    for size in [10, 100, 1000].iter() {
        let data = generate_sample_jsonl(*size);

        group.bench_with_input(BenchmarkId::new("build_tree", size), &data, |b, data| {
            // Pre-parse entries
            let mut parser = JsonlParser::new();
            let entries = parser.parse_str(data).unwrap();

            b.iter(|| {
                let conversation = Conversation::from_entries(entries.clone());
                black_box(conversation)
            });
        });
    }

    group.finish();
}

fn bench_export(c: &mut Criterion) {
    let data = generate_sample_jsonl(100);
    let mut parser = JsonlParser::new();
    let entries = parser.parse_str(&data).unwrap();
    let conversation = Conversation::from_entries(entries).unwrap();
    let options = ExportOptions::default();

    let mut group = c.benchmark_group("export");

    group.bench_function("json", |b| {
        b.iter(|| {
            let mut output = Vec::new();
            let exporter = JsonExporter::new();
            exporter
                .export_conversation(&conversation, &mut output, &options)
                .unwrap();
            black_box(output)
        });
    });

    group.bench_function("markdown", |b| {
        b.iter(|| {
            let mut output = Vec::new();
            let exporter = MarkdownExporter::new();
            exporter
                .export_conversation(&conversation, &mut output, &options)
                .unwrap();
            black_box(output)
        });
    });

    group.bench_function("text", |b| {
        b.iter(|| {
            let mut output = Vec::new();
            let exporter = TextExporter::new();
            exporter
                .export_conversation(&conversation, &mut output, &options)
                .unwrap();
            black_box(output)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_parser, bench_reconstruction, bench_export);
criterion_main!(benches);
