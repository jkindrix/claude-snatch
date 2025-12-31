//! Snapshot tests for export formats using insta.
//!
//! These tests verify that export output remains consistent across changes.
//! Run `cargo insta review` to update snapshots after intentional changes.

use std::io::Cursor;
use std::path::PathBuf;

use claude_snatch::export::{
    CsvExporter, ExportOptions, Exporter, JsonExporter, MarkdownExporter, TextExporter,
    XmlExporter,
};
use claude_snatch::model::LogEntry;
use claude_snatch::parser::JsonlParser;
use insta::assert_snapshot;

/// Get the path to a fixture file.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Parse a fixture file and return log entries.
fn parse_fixture(name: &str) -> Vec<LogEntry> {
    let path = fixture_path(name);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read fixture {}: {}", name, e));

    let mut parser = JsonlParser::new();
    parser
        .parse_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse fixture {}: {}", name, e))
}

/// Default export options for consistent snapshots.
fn default_options() -> ExportOptions {
    ExportOptions::default()
}

/// Export entries to string using the given exporter.
fn export_to_string<E: Exporter>(exporter: &E, entries: &[LogEntry]) -> String {
    let mut buffer = Cursor::new(Vec::new());
    exporter
        .export_entries(entries, &mut buffer, &default_options())
        .expect("Export failed");
    String::from_utf8(buffer.into_inner()).expect("Invalid UTF-8 in export output")
}

// =============================================================================
// JSON Export Snapshots
// =============================================================================

#[test]
fn json_simple() {
    let entries = parse_fixture("simple_session.jsonl");
    let exporter = JsonExporter::new().pretty(true).with_envelope(false);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

#[test]
fn json_thinking() {
    let entries = parse_fixture("thinking_session.jsonl");
    let exporter = JsonExporter::new().pretty(true).with_envelope(false);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

// =============================================================================
// Text Export Snapshots
// =============================================================================

#[test]
fn text_simple() {
    let entries = parse_fixture("simple_session.jsonl");
    let exporter = TextExporter::new();
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

#[test]
fn text_thinking() {
    let entries = parse_fixture("thinking_session.jsonl");
    let exporter = TextExporter::new();
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

// =============================================================================
// CSV Export Snapshots
// =============================================================================

#[test]
fn csv_simple() {
    let entries = parse_fixture("simple_session.jsonl");
    let exporter = CsvExporter::new();
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

// =============================================================================
// XML Export Snapshots
// =============================================================================

#[test]
fn xml_simple() {
    let entries = parse_fixture("simple_session.jsonl");
    let exporter = XmlExporter::new().pretty(true);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

#[test]
fn xml_thinking() {
    let entries = parse_fixture("thinking_session.jsonl");
    let exporter = XmlExporter::new().pretty(true);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

// =============================================================================
// Markdown Export Snapshots
// =============================================================================

#[test]
fn markdown_simple() {
    let entries = parse_fixture("simple_session.jsonl");
    let exporter = MarkdownExporter::new().with_toc(false);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

#[test]
fn markdown_thinking() {
    let entries = parse_fixture("thinking_session.jsonl");
    let exporter = MarkdownExporter::new().with_toc(false);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

// =============================================================================
// Branching Session Snapshots
// =============================================================================

#[test]
fn json_branching() {
    let entries = parse_fixture("branching_session.jsonl");
    let exporter = JsonExporter::new().pretty(true).with_envelope(false);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

#[test]
fn text_branching() {
    let entries = parse_fixture("branching_session.jsonl");
    let exporter = TextExporter::new();
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

// =============================================================================
// System Session Snapshots
// =============================================================================

#[test]
fn json_system() {
    let entries = parse_fixture("system_session.jsonl");
    let exporter = JsonExporter::new().pretty(true).with_envelope(false);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}

#[test]
fn markdown_system() {
    let entries = parse_fixture("system_session.jsonl");
    let exporter = MarkdownExporter::new().with_toc(false);
    let output = export_to_string(&exporter, &entries);
    assert_snapshot!(output);
}
