//! Integration tests for claude-snatch.
//!
//! These tests verify the full parsing and export pipeline using
//! sample JSONL fixtures and synthetic test data generators.

use claude_snatch::model::{ContentBlock, LogEntry};
use claude_snatch::parser::JsonlParser;
use claude_snatch::reconstruction::Conversation;
use std::path::PathBuf;

mod generators;

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

mod parsing {
    use super::*;

    #[test]
    fn test_parse_simple_session() {
        let entries = parse_fixture("simple_session.jsonl");

        assert_eq!(entries.len(), 6, "Expected 6 entries in simple session");

        // Verify message types
        assert!(matches!(entries[0], LogEntry::User(_)));
        assert!(matches!(entries[1], LogEntry::Assistant(_)));
        assert!(matches!(entries[2], LogEntry::User(_)));
        assert!(matches!(entries[3], LogEntry::Assistant(_)));
        assert!(matches!(entries[4], LogEntry::User(_)));
        assert!(matches!(entries[5], LogEntry::Assistant(_)));
    }

    #[test]
    fn test_parse_thinking_session() {
        let entries = parse_fixture("thinking_session.jsonl");

        assert_eq!(entries.len(), 2, "Expected 2 entries in thinking session");

        // Verify thinking block exists
        if let LogEntry::Assistant(assistant) = &entries[1] {
            let has_thinking = assistant
                .message
                .content
                .iter()
                .any(|c| matches!(c, ContentBlock::Thinking(_)));
            assert!(has_thinking, "Expected thinking block in response");
        } else {
            panic!("Expected assistant message at index 1");
        }
    }

    #[test]
    fn test_parse_branching_session() {
        let entries = parse_fixture("branching_session.jsonl");

        assert_eq!(entries.len(), 6, "Expected 6 entries in branching session");

        // Check for sidechain marker
        let has_sidechain = entries.iter().any(|e| {
            if let LogEntry::User(user) = e {
                user.is_sidechain
            } else {
                false
            }
        });
        assert!(has_sidechain, "Expected at least one sidechain message");
    }

    #[test]
    fn test_parse_system_session() {
        let entries = parse_fixture("system_session.jsonl");

        // Verify system message exists
        let has_system = entries.iter().any(|e| matches!(e, LogEntry::System(_)));
        assert!(has_system, "Expected system message");

        // Verify summary exists
        let has_summary = entries.iter().any(|e| matches!(e, LogEntry::Summary(_)));
        assert!(has_summary, "Expected summary message");
    }

    #[test]
    fn test_tool_use_parsing() {
        let entries = parse_fixture("simple_session.jsonl");

        // Find the assistant message with tool_use
        let tool_use_entry = entries
            .iter()
            .find(|e| {
                if let LogEntry::Assistant(a) = e {
                    a.message
                        .content
                        .iter()
                        .any(|c| matches!(c, ContentBlock::ToolUse(_)))
                } else {
                    false
                }
            })
            .expect("Expected an assistant message with tool_use");

        if let LogEntry::Assistant(assistant) = tool_use_entry {
            let tool_use = assistant
                .message
                .content
                .iter()
                .find_map(|c| {
                    if let ContentBlock::ToolUse(tu) = c {
                        Some(tu)
                    } else {
                        None
                    }
                })
                .expect("Expected tool_use content block");

            assert_eq!(tool_use.name, "Bash");
            assert!(!tool_use.id.is_empty());
        }
    }

    #[test]
    fn test_tool_result_parsing() {
        let entries = parse_fixture("simple_session.jsonl");

        // Find user message with tool_result
        let has_tool_result = entries.iter().any(|e| {
            if let LogEntry::User(user) = e {
                user.tool_use_result.is_some()
            } else {
                false
            }
        });

        assert!(has_tool_result, "Expected a user message with toolUseResult");
    }
}

mod reconstruction {
    use super::*;

    #[test]
    fn test_simple_conversation_tree() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        assert!(!conversation.is_empty());
        assert_eq!(conversation.roots().len(), 1, "Expected single root");
        assert!(!conversation.has_branches(), "Simple session should not branch");
    }

    #[test]
    fn test_branching_conversation_tree() {
        let entries = parse_fixture("branching_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        assert!(
            conversation.has_branches(),
            "Branching session should have branches"
        );
        assert!(
            conversation.branch_points().len() >= 1,
            "Expected at least one branch point"
        );
    }

    #[test]
    fn test_main_thread_extraction() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let main_thread = conversation.main_thread_entries();
        assert!(!main_thread.is_empty(), "Main thread should not be empty");

        // Main thread should follow parent chain
        for i in 1..main_thread.len() {
            let parent_uuid = main_thread[i].parent_uuid();
            let prev_uuid = main_thread[i - 1].uuid().unwrap();

            assert_eq!(
                parent_uuid,
                Some(prev_uuid),
                "Entry {} should have entry {} as parent",
                i,
                i - 1
            );
        }
    }

    #[test]
    fn test_conversation_statistics() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let stats = conversation.statistics();

        assert!(stats.user_messages > 0, "Expected user messages");
        assert!(stats.assistant_messages > 0, "Expected assistant messages");
        assert!(stats.tool_uses > 0, "Expected tool uses");
        assert!(stats.tool_results > 0, "Expected tool results");
        assert!(stats.tools_balanced(), "Tool uses and results should be balanced");
    }

    #[test]
    fn test_thinking_block_statistics() {
        let entries = parse_fixture("thinking_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let stats = conversation.statistics();
        assert!(
            stats.thinking_blocks > 0,
            "Expected thinking blocks in thinking session"
        );
    }
}

mod export {
    use super::*;
    use claude_snatch::export::{ExportOptions, HtmlExporter, JsonExporter, MarkdownExporter, Exporter};

    #[test]
    fn test_json_export() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries.clone()).expect("Failed to build conversation");

        let exporter = JsonExporter::new().pretty(true).with_envelope(false);
        let options = ExportOptions::default();
        let mut output = Vec::new();

        exporter
            .export_conversation(&conversation, &mut output, &options)
            .expect("Failed to export to JSON");

        let output_str = String::from_utf8(output).expect("Invalid UTF-8");

        // Verify output is valid JSON array
        let parsed: serde_json::Value =
            serde_json::from_str(&output_str).expect("Output should be valid JSON");
        assert!(parsed.is_array(), "Output should be JSON array");
    }

    #[test]
    fn test_markdown_export() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let exporter = MarkdownExporter::new();
        let options = ExportOptions::default()
            .with_metadata(true);

        let mut output = Vec::new();
        exporter
            .export_conversation(&conversation, &mut output, &options)
            .expect("Failed to export to Markdown");

        let output_str = String::from_utf8(output).expect("Invalid UTF-8");

        // Verify markdown structure
        assert!(output_str.contains("User") || output_str.contains("user"), "Should have User content");
        assert!(
            output_str.contains("Assistant") || output_str.contains("assistant"),
            "Should have Assistant content"
        );
    }

    #[test]
    fn test_json_round_trip() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries.clone()).expect("Failed to build conversation");

        let exporter = JsonExporter::new().with_envelope(false);
        let options = ExportOptions::default();
        let mut output = Vec::new();

        exporter
            .export_conversation(&conversation, &mut output, &options)
            .expect("Failed to export to JSON");

        let output_str = String::from_utf8(output).expect("Invalid UTF-8");

        // Parse the output back
        let _parsed: Vec<serde_json::Value> =
            serde_json::from_str(&output_str).expect("Should parse exported JSON");
    }

    #[test]
    fn test_export_with_thinking() {
        let entries = parse_fixture("thinking_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let exporter = MarkdownExporter::new();
        let options = ExportOptions::default().with_thinking(true);

        let mut output = Vec::new();
        exporter
            .export_conversation(&conversation, &mut output, &options)
            .expect("Failed to export to Markdown");

        let output_str = String::from_utf8(output).expect("Invalid UTF-8");

        // Should include thinking content
        assert!(
            output_str.contains("thinking") || output_str.contains("Thinking") || output_str.contains("345"),
            "Should include thinking-related content"
        );
    }

    #[test]
    fn test_html_export() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let exporter = HtmlExporter::new().with_title("Test Export");
        let options = ExportOptions::default();
        let mut output = Vec::new();

        exporter
            .export_conversation(&conversation, &mut output, &options)
            .expect("Failed to export to HTML");

        let output_str = String::from_utf8(output).expect("Invalid UTF-8");

        // Verify HTML structure
        assert!(output_str.contains("<!DOCTYPE html>"), "Should have DOCTYPE");
        assert!(output_str.contains("<html"), "Should have html tag");
        assert!(output_str.contains("</html>"), "Should close html tag");
        assert!(output_str.contains("<title>Test Export</title>"), "Should have custom title");
        assert!(output_str.contains("message-user"), "Should have user message");
        assert!(output_str.contains("message-assistant"), "Should have assistant message");
    }

    #[test]
    fn test_html_export_dark_theme() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let exporter = HtmlExporter::new().dark_theme(true);
        let options = ExportOptions::default();
        let mut output = Vec::new();

        exporter
            .export_conversation(&conversation, &mut output, &options)
            .expect("Failed to export to HTML");

        let output_str = String::from_utf8(output).expect("Invalid UTF-8");

        // Should use dark class
        assert!(output_str.contains("class=\"dark\""), "Should use dark theme class");
    }

    #[test]
    fn test_plain_text_export() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let exporter = MarkdownExporter::new().plain_text(true);
        let options = ExportOptions::default();
        let mut output = Vec::new();

        exporter
            .export_conversation(&conversation, &mut output, &options)
            .expect("Failed to export to Plain Text");

        let output_str = String::from_utf8(output).expect("Invalid UTF-8");

        // Plain text should not contain markdown formatting
        assert!(!output_str.contains("##"), "Should not have markdown headers");
        assert!(!output_str.contains("```"), "Should not have code fences");
        assert!(output_str.contains("User") || output_str.contains("user"), "Should have user label");
    }
}

mod analytics {
    use super::*;
    use claude_snatch::analytics::SessionAnalytics;

    #[test]
    fn test_session_analytics() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let analytics = SessionAnalytics::from_conversation(&conversation);

        assert!(analytics.message_counts.total() > 0);
        assert!(analytics.message_counts.user > 0);
        assert!(analytics.message_counts.assistant > 0);
    }

    #[test]
    fn test_token_usage() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let analytics = SessionAnalytics::from_conversation(&conversation);
        let summary = analytics.summary_report();

        assert!(summary.input_tokens > 0, "Expected input tokens");
        assert!(summary.output_tokens > 0, "Expected output tokens");
    }

    #[test]
    fn test_tool_statistics() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).expect("Failed to build conversation");

        let analytics = SessionAnalytics::from_conversation(&conversation);

        assert!(
            !analytics.tool_counts.is_empty(),
            "Expected tool usage in simple session"
        );
        assert!(
            analytics.tool_counts.contains_key("Bash"),
            "Expected Bash tool usage"
        );
    }
}

mod edge_cases {
    use claude_snatch::parser::JsonlParser;

    #[test]
    fn test_empty_input() {
        let mut parser = JsonlParser::new();
        let result = parser.parse_str("");

        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_whitespace_only() {
        let mut parser = JsonlParser::new();
        let result = parser.parse_str("   \n\n   \n");

        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_malformed_line_recovery() {
        let mut parser = JsonlParser::new().with_lenient(true);

        // Valid user messages require uuid, timestamp, sessionId, version, message
        let input = r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:00.000Z","sessionId":"test","version":"2.0.74","message":{"role":"user","content":"hello"}}
not valid json here
{"type":"user","uuid":"22222222-2222-2222-2222-222222222222","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"test","version":"2.0.74","message":{"role":"user","content":"world"}}"#;

        let result = parser.parse_str(input);
        assert!(result.is_ok(), "Lenient parser should recover from errors");

        let entries = result.unwrap();
        // Should parse at least one valid entry
        assert!(!entries.is_empty(), "Expected at least one valid entry");

        // In lenient mode, should skip the bad line and parse the valid ones
        let stats = parser.stats();
        assert!(stats.lines_skipped > 0 || stats.errors.len() > 0, "Should have skipped bad line");
    }

    #[test]
    fn test_unknown_message_type() {
        let mut parser = JsonlParser::new().with_lenient(true);

        let input = r#"{"type":"future_type","uuid":"1","data":"something new"}"#;

        let result = parser.parse_str(input);
        // Should handle unknown types gracefully
        assert!(result.is_ok());
    }
}

mod generated_data {
    use super::generators::{generate_session, SessionConfig};
    use claude_snatch::parser::JsonlParser;
    use claude_snatch::reconstruction::Conversation;
    use claude_snatch::analytics::SessionAnalytics;
    use claude_snatch::export::{ExportOptions, JsonExporter, MarkdownExporter, Exporter};

    #[test]
    fn test_parse_generated_minimal_session() {
        let config = SessionConfig::minimal();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        // 3 exchanges = 6 messages
        assert_eq!(entries.len(), 6);
    }

    #[test]
    fn test_parse_generated_with_thinking() {
        let config = SessionConfig {
            exchanges: 5,
            include_thinking: true,
            include_tools: false,
            avg_text_length: 100,
            avg_thinking_length: 200,
            ..Default::default()
        };
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let conversation = Conversation::from_entries(entries).unwrap();
        let stats = conversation.statistics();

        assert!(stats.thinking_blocks > 0, "Expected thinking blocks");
    }

    #[test]
    fn test_parse_generated_with_tools() {
        let config = SessionConfig {
            exchanges: 10,
            include_thinking: false,
            include_tools: true,
            ..Default::default()
        };
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let conversation = Conversation::from_entries(entries).unwrap();
        let stats = conversation.statistics();

        assert!(stats.tool_uses > 0, "Expected tool uses");
    }

    #[test]
    fn test_export_generated_session_json() {
        let config = SessionConfig::default();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let conversation = Conversation::from_entries(entries).unwrap();

        let exporter = JsonExporter::new().pretty(true).with_envelope(false);
        let options = ExportOptions::default();
        let mut output = Vec::new();

        exporter.export_conversation(&conversation, &mut output, &options).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let _: serde_json::Value = serde_json::from_str(&output_str).unwrap();
    }

    #[test]
    fn test_export_generated_session_markdown() {
        let config = SessionConfig::default();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let conversation = Conversation::from_entries(entries).unwrap();

        let exporter = MarkdownExporter::new();
        let options = ExportOptions::default().with_thinking(true);
        let mut output = Vec::new();

        exporter.export_conversation(&conversation, &mut output, &options).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        assert!(!output_str.is_empty());
    }

    #[test]
    fn test_analytics_on_generated_session() {
        let config = SessionConfig {
            exchanges: 20,
            include_thinking: true,
            include_tools: true,
            ..Default::default()
        };
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let conversation = Conversation::from_entries(entries).unwrap();
        let analytics = SessionAnalytics::from_conversation(&conversation);
        let summary = analytics.summary_report();

        assert_eq!(summary.user_messages, 20);
        assert_eq!(summary.assistant_messages, 20);
        assert!(summary.input_tokens > 0);
        assert!(summary.output_tokens > 0);
    }
}

mod large_file_handling {
    use super::generators::{generate_session, generate_large_file, SessionConfig};
    use claude_snatch::parser::JsonlParser;
    use claude_snatch::reconstruction::Conversation;
    use claude_snatch::export::{ExportOptions, JsonExporter, Exporter};
    use std::time::Instant;

    #[test]
    fn test_parse_large_session() {
        // Generate a large session with 100 exchanges
        let config = SessionConfig::large();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        // Parse first assistant line manually for debug
        if let Some(assistant_line) = lines.iter().find(|l| l.contains("\"type\":\"assistant\"")) {
            eprintln!("Sample assistant line:");
            eprintln!("{}", &assistant_line[..200.min(assistant_line.len())]);

            // Try parsing it directly
            match claude_snatch::parser::JsonlParser::parse_entry(assistant_line) {
                Ok(_) => eprintln!("Direct parse: SUCCESS"),
                Err(e) => eprintln!("Direct parse: FAILED - {}", e),
            }
        }

        let start = Instant::now();

        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let parse_time = start.elapsed();

        // Show parse stats
        let stats = parser.stats();
        eprintln!("Parsed {} entries, skipped {}, errors: {}",
            stats.entries_parsed, stats.lines_skipped, stats.errors.len());
        for (i, err) in stats.errors.iter().take(3).enumerate() {
            eprintln!("Error {}: line {} - {}", i, err.line, err.message);
        }

        // 100 exchanges = 200 messages
        assert_eq!(entries.len(), 200, "Expected 200 entries from 100 exchanges");

        // Parsing should be fast (under 1 second for 200 messages)
        assert!(parse_time.as_secs() < 1, "Parsing took too long: {:?}", parse_time);
    }

    #[test]
    fn test_parse_huge_session() {
        // Generate a huge session with 500 exchanges
        let config = SessionConfig {
            exchanges: 500,
            include_thinking: true,
            include_tools: true,
            avg_text_length: 300,
            avg_thinking_length: 1000,
            ..Default::default()
        };
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let size_mb = content.len() as f64 / 1_000_000.0;
        eprintln!("Generated session size: {:.2} MB", size_mb);

        let start = Instant::now();

        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let parse_time = start.elapsed();

        assert_eq!(entries.len(), 1000);

        // Parsing should complete in reasonable time
        assert!(parse_time.as_secs() < 5, "Parsing took too long: {:?}", parse_time);

        eprintln!("Parsed {} entries in {:?}", entries.len(), parse_time);
    }

    #[test]
    fn test_reconstruct_large_conversation() {
        let config = SessionConfig::large();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let start = Instant::now();
        let conversation = Conversation::from_entries(entries).unwrap();
        let reconstruct_time = start.elapsed();

        assert_eq!(conversation.len(), 200);
        assert!(reconstruct_time.as_secs() < 1, "Reconstruction took too long: {:?}", reconstruct_time);
    }

    #[test]
    fn test_export_large_session_json() {
        let config = SessionConfig::large();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let conversation = Conversation::from_entries(entries).unwrap();

        let exporter = JsonExporter::new().with_envelope(false);
        let options = ExportOptions::default();
        let mut output = Vec::new();

        let start = Instant::now();
        exporter.export_conversation(&conversation, &mut output, &options).unwrap();
        let export_time = start.elapsed();

        assert!(export_time.as_secs() < 2, "Export took too long: {:?}", export_time);

        let output_size = output.len();
        eprintln!("Exported {} bytes in {:?}", output_size, export_time);
    }

    #[test]
    fn test_multi_session_file() {
        // Generate multiple sessions in one file
        let mut buffer = Vec::new();
        generate_large_file(10, 10, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let size_kb = content.len() as f64 / 1000.0;
        eprintln!("Multi-session file size: {:.2} KB", size_kb);

        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        // 10 sessions * 10 exchanges * 2 messages = 200
        assert_eq!(entries.len(), 200);
    }

    #[test]
    #[ignore] // Run with --ignored for stress testing
    fn stress_test_very_large_file() {
        // Generate a very large file for stress testing
        let config = SessionConfig::huge();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let size_mb = content.len() as f64 / 1_000_000.0;
        eprintln!("Very large session size: {:.2} MB", size_mb);

        let start = Instant::now();

        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(&content).unwrap();

        let conversation = Conversation::from_entries(entries.clone()).unwrap();

        let total_time = start.elapsed();

        eprintln!(
            "Processed {} entries ({:.2} MB) in {:?}",
            entries.len(),
            size_mb,
            total_time
        );

        assert_eq!(entries.len(), 2000);
        assert_eq!(conversation.len(), 2000);
    }
}

/// Property-based tests using proptest.
///
/// These tests verify parser robustness against arbitrary input.
mod property_tests {
    use proptest::prelude::*;

    use claude_snatch::parser::JsonlParser;

    proptest! {
        /// Parser should never panic on arbitrary byte sequences.
        #[test]
        fn parser_never_panics_on_arbitrary_input(input in any::<Vec<u8>>()) {
            let content = String::from_utf8_lossy(&input);
            let mut parser = JsonlParser::new().with_lenient(true);
            // Should not panic, even on malformed input
            let _ = parser.parse_str(&content);
        }

        /// Parser handles arbitrary valid-looking JSONL lines without panic.
        #[test]
        fn parser_handles_json_like_strings(
            uuid in "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}",
            content in ".*"
        ) {
            let json = format!(
                r#"{{"uuid":"{}","type":"user","message":{{"role":"user","content":"{}"}}}}"#,
                uuid, content.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let mut parser = JsonlParser::new().with_lenient(true);
            let _ = parser.parse_str(&json);
        }

        /// Empty lines and whitespace-only lines are always skipped.
        #[test]
        fn parser_skips_whitespace_lines(
            spaces in "[ \t\n\r]*",
            valid_json in prop::bool::ANY
        ) {
            let content = if valid_json {
                format!(
                    "{}\n{{\n\"uuid\":\"test\",\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"test\"}}}}\n{}",
                    spaces, spaces
                )
            } else {
                spaces.clone()
            };
            let mut parser = JsonlParser::new().with_lenient(true);
            let result = parser.parse_str(&content);
            // Should succeed without panic
            assert!(result.is_ok());
        }

        /// Lenient mode should always return Ok, even with bad input.
        #[test]
        fn lenient_mode_always_succeeds(input in ".*") {
            let mut parser = JsonlParser::new().with_lenient(true);
            let result = parser.parse_str(&input);
            assert!(result.is_ok(), "Lenient mode should always succeed");
        }

        /// Parse stats are consistent with the number of lines.
        #[test]
        fn parse_stats_are_consistent(lines in prop::collection::vec(".*", 0..20)) {
            let content = lines.join("\n");
            let mut parser = JsonlParser::new().with_lenient(true);
            let entries = parser.parse_str(&content).unwrap();
            let stats = parser.stats();

            // Total lines = entries parsed + skipped + empty
            let accounted = stats.entries_parsed + stats.lines_skipped + stats.empty_lines;
            assert_eq!(
                stats.lines_processed, accounted,
                "Stats should account for all lines: processed={}, accounted={}",
                stats.lines_processed, accounted
            );

            // Entries returned should match entries_parsed
            assert_eq!(
                entries.len(), stats.entries_parsed,
                "Returned entries should match stats"
            );
        }
    }
}

/// Snapshot tests using insta.
///
/// These tests capture expected output formats to detect regressions.
mod snapshot_tests {
    use super::*;
    use claude_snatch::export::{ExportOptions, Exporter, MarkdownExporter, JsonExporter};

    #[test]
    fn test_markdown_export_snapshot() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).unwrap();

        let exporter = MarkdownExporter::new();
        let options = ExportOptions::default();
        let mut output = Vec::new();
        exporter.export_conversation(&conversation, &mut output, &options).unwrap();
        let markdown = String::from_utf8(output).unwrap();

        insta::assert_snapshot!("simple_session_markdown", markdown);
    }

    #[test]
    fn test_json_export_snapshot() {
        let entries = parse_fixture("simple_session.jsonl");
        let conversation = Conversation::from_entries(entries).unwrap();

        let exporter = JsonExporter::new().pretty(true);
        let options = ExportOptions::default();
        let mut output = Vec::new();
        exporter.export_conversation(&conversation, &mut output, &options).unwrap();
        let json = String::from_utf8(output).unwrap();

        // Redact the timestamp which changes on each run
        let mut settings = insta::Settings::clone_current();
        settings.add_filter(r#""exported_at": "[^"]+""#, r#""exported_at": "[REDACTED]""#);
        settings.bind(|| {
            insta::assert_snapshot!("simple_session_json", json);
        });
    }

    #[test]
    fn test_thinking_session_markdown_snapshot() {
        let entries = parse_fixture("thinking_session.jsonl");
        let conversation = Conversation::from_entries(entries).unwrap();

        let exporter = MarkdownExporter::new();
        let options = ExportOptions::default().with_thinking(true);
        let mut output = Vec::new();
        exporter.export_conversation(&conversation, &mut output, &options).unwrap();
        let markdown = String::from_utf8(output).unwrap();

        insta::assert_snapshot!("thinking_session_markdown", markdown);
    }

    #[test]
    fn test_branching_session_main_thread_snapshot() {
        let entries = parse_fixture("branching_session.jsonl");
        let conversation = Conversation::from_entries(entries).unwrap();

        let exporter = MarkdownExporter::new();
        // main_thread_only is true by default in ExportOptions
        let options = ExportOptions::default();
        let mut output = Vec::new();
        exporter.export_conversation(&conversation, &mut output, &options).unwrap();
        let markdown = String::from_utf8(output).unwrap();

        insta::assert_snapshot!("branching_session_main_thread", markdown);
    }
}
