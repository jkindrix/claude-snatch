//! Property-based tests for the JSONL parser.
//!
//! Uses proptest to fuzz the parser with generated inputs to ensure
//! it handles arbitrary data without panicking.

use claude_snatch::parser::JsonlParser;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Parser should never panic on arbitrary byte input.
    #[test]
    fn parser_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..10000)) {
        let content = String::from_utf8_lossy(&bytes);
        let mut parser = JsonlParser::new().with_lenient(true);
        // Should not panic, may return Ok or Err
        let _ = parser.parse_str(&content);
    }

    /// Parser should handle arbitrary valid UTF-8 strings.
    #[test]
    fn parser_handles_arbitrary_utf8(content in ".*") {
        let mut parser = JsonlParser::new().with_lenient(true);
        let _ = parser.parse_str(&content);
    }

    /// Parser should handle multiple lines of arbitrary content.
    #[test]
    fn parser_handles_multiline_garbage(
        lines in prop::collection::vec(".*", 0..100)
    ) {
        let content = lines.join("\n");
        let mut parser = JsonlParser::new().with_lenient(true);
        let result = parser.parse_str(&content);

        // In lenient mode, should always succeed (may return empty vec)
        prop_assert!(result.is_ok());
    }

    /// Parser stats should be consistent.
    #[test]
    fn parser_stats_are_consistent(
        lines in prop::collection::vec("[^\n]*", 1..50)
    ) {
        let content = lines.join("\n");
        let mut parser = JsonlParser::new().with_lenient(true);
        let result = parser.parse_str(&content);

        prop_assert!(result.is_ok());

        let stats = parser.stats();
        // lines_processed should equal entries_parsed + lines_skipped + empty_lines
        prop_assert_eq!(
            stats.lines_processed,
            stats.entries_parsed + stats.lines_skipped + stats.empty_lines,
            "Stats don't add up: processed={}, parsed={}, skipped={}, empty={}",
            stats.lines_processed,
            stats.entries_parsed,
            stats.lines_skipped,
            stats.empty_lines
        );
    }

    /// Parser should reject obviously invalid JSON in strict mode.
    #[test]
    fn strict_parser_rejects_invalid_json(content in "[^{}\\[\\]\"]+") {
        // Content without JSON structural characters
        if content.trim().is_empty() {
            return Ok(()); // Skip empty content
        }

        let mut parser = JsonlParser::new().with_lenient(false);
        let result = parser.parse_str(&content);

        // Should fail on non-JSON content
        prop_assert!(result.is_err());
    }

    /// Valid JSON objects should parse or fail gracefully.
    #[test]
    fn valid_json_objects_handled(
        key in "[a-zA-Z_][a-zA-Z0-9_]*",
        value in "[a-zA-Z0-9 ]*"
    ) {
        let json = format!(r#"{{"{}": "{}"}}"#, key, value);
        let mut parser = JsonlParser::new().with_lenient(true);
        let _ = parser.parse_str(&json);
        // May parse as Unknown type or skip - either is acceptable
    }

    /// Empty lines should be counted correctly.
    #[test]
    fn empty_lines_counted(
        empty_count in 0usize..20,
        content_lines in prop::collection::vec("[^\n]+", 0..10)
    ) {
        let mut lines: Vec<String> = Vec::new();

        // Interleave empty lines with content
        for (i, content) in content_lines.iter().enumerate() {
            if i < empty_count {
                lines.push(String::new());
            }
            lines.push(content.clone());
        }
        // Add remaining empty lines
        for _ in content_lines.len()..empty_count {
            lines.push(String::new());
        }

        let content = lines.join("\n");
        let mut parser = JsonlParser::new().with_lenient(true);
        let _ = parser.parse_str(&content);

        // Count whitespace-only content lines (they're also counted as empty by parser)
        let whitespace_only = content_lines.iter()
            .filter(|s| s.trim().is_empty())
            .count();

        // Empty lines should be tracked (includes explicit empty + whitespace-only)
        let stats = parser.stats();
        prop_assert!(stats.empty_lines <= empty_count + whitespace_only + 1); // +1 for potential trailing
    }

    /// Parser should handle deeply nested JSON without stack overflow.
    #[test]
    fn handles_deep_nesting(depth in 1usize..100) {
        let open = "{\"a\":".repeat(depth);
        let close = "}".repeat(depth);
        let json = format!("{}\"value\"{}", open, close);

        let mut parser = JsonlParser::new().with_lenient(true);
        let _ = parser.parse_str(&json);
        // Should not stack overflow
    }

    /// Parser should handle very long lines.
    #[test]
    fn handles_long_lines(length in 1000usize..100000) {
        let content = "a".repeat(length);
        let mut parser = JsonlParser::new().with_lenient(true);
        let _ = parser.parse_str(&content);
        // Should not panic or hang
    }

    /// Success rate should be between 0 and 100.
    #[test]
    fn success_rate_bounds(
        lines in prop::collection::vec(".*", 0..50)
    ) {
        let content = lines.join("\n");
        let mut parser = JsonlParser::new().with_lenient(true);
        let _ = parser.parse_str(&content);

        let rate = parser.stats().success_rate();
        prop_assert!(rate >= 0.0 && rate <= 100.0, "Rate out of bounds: {}", rate);
    }
}

/// Tests for specific edge cases discovered through fuzzing.
mod edge_cases {
    use super::*;

    #[test]
    fn null_bytes_in_content() {
        let content = "hello\0world";
        let mut parser = JsonlParser::new().with_lenient(true);
        let result = parser.parse_str(content);
        assert!(result.is_ok());
    }

    #[test]
    fn unicode_edge_cases() {
        let cases = [
            "\u{FEFF}", // BOM
            "\u{200B}", // Zero-width space
            "\u{FFFD}", // Replacement character
            "🦀",       // Emoji
            "日本語",   // CJK
            "\u{1F600}\u{1F601}\u{1F602}", // Multiple emoji
        ];

        for content in cases {
            let mut parser = JsonlParser::new().with_lenient(true);
            let result = parser.parse_str(content);
            assert!(result.is_ok(), "Failed on: {:?}", content);
        }
    }

    #[test]
    fn control_characters() {
        let content = "\t\r\n\x0B\x0C";
        let mut parser = JsonlParser::new().with_lenient(true);
        let result = parser.parse_str(content);
        assert!(result.is_ok());
    }

    #[test]
    fn very_long_single_line() {
        let content = "x".repeat(10_000_000); // 10MB line
        let mut parser = JsonlParser::new().with_lenient(true);
        let result = parser.parse_str(&content);
        assert!(result.is_ok());
    }

    #[test]
    fn many_empty_lines() {
        let content = "\n".repeat(10_000);
        let mut parser = JsonlParser::new().with_lenient(true);
        let result = parser.parse_str(&content);
        assert!(result.is_ok());
        assert_eq!(parser.stats().empty_lines, 10_000);
    }
}
