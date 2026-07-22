//! Code extraction command.
//!
//! Extracts code blocks from Claude Code session conversations.

use std::io::Write;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::cli::{Cli, CodeArgs, OutputFormat};
use crate::error::Result;
use crate::export::extract_code_blocks;
use crate::model::{ContentBlock, LogEntry, UserContent};
use crate::provider::PromptAuthorship;
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Extracted code with metadata.
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedCode {
    /// Programming language (if specified).
    pub language: Option<String>,
    /// The code content.
    pub code: String,
    /// Source message type (user/assistant).
    pub source: String,
    /// Message timestamp.
    pub timestamp: Option<DateTime<Utc>>,
    /// Block index within the session.
    pub index: usize,
    /// Source provider on explicitly routed calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Provider-qualified source session on explicitly routed calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_id: Option<String>,
}

#[derive(Debug, Clone)]
struct CodeSource {
    provider: Option<String>,
    qualified_id: Option<String>,
    native_id: String,
    semantic_annotations: bool,
}

/// Run the code extraction command.
pub fn run(cli: &Cli, args: &CodeArgs) -> Result<()> {
    let output_format = cli.effective_output();
    let registry = (!args.provider.is_empty() || args.session.contains(':'))
        .then(|| super::helpers::provider_registry(cli));
    let provider_route = !args.provider.is_empty()
        || registry
            .as_ref()
            .is_some_and(|registry| registry.looks_qualified(&args.session));
    let (conversation, source) = if provider_route {
        // Complete classification: a future CodeArgs field must be
        // consciously supported or refused on this route.
        let CodeArgs {
            session: _,
            provider: _,
            lang: _,
            assistant_only: _,
            user_only: _,
            limit: _,
            main_thread: _,
            metadata: _,
            concatenate: _,
            files: _,
            output_dir: _,
            quiet: _,
        } = args;
        let registry =
            registry.expect("provider flags or qualified reference constructed registry");
        let resolution = registry.resolve_with_default_policy(&args.provider, &args.session)?;
        let parsed = crate::provider::registry::cached_parsed_session(
            crate::cache::global_cache(),
            resolution.provider,
            &resolution.key,
        )?;
        let source = CodeSource {
            provider: Some(resolution.key.provider.to_string()),
            qualified_id: Some(resolution.key.to_string()),
            native_id: resolution.key.native_id.clone(),
            semantic_annotations: resolution.provider.capabilities().semantic_annotations,
        };
        (Conversation::from_parsed_session(parsed)?, source)
    } else {
        let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
        let sessions = claude_dir.all_sessions()?;
        let session = sessions
            .iter()
            .find(|s| s.session_id().starts_with(&args.session) || s.session_id() == args.session)
            .ok_or_else(|| crate::error::SnatchError::SessionNotFound {
                session_id: args.session.clone(),
            })?;
        let entries = session.parse_with_options(cli.max_file_size)?;
        (
            Conversation::from_entries(entries)?,
            CodeSource {
                provider: None,
                qualified_id: None,
                native_id: session.session_id().to_string(),
                semantic_annotations: false,
            },
        )
    };

    // Extract code blocks
    let mut extracted = extract_code_from_conversation(&conversation, args, &source);

    // Filter by language if specified
    if let Some(ref lang) = args.lang {
        let lang_lower = lang.to_lowercase();
        extracted.retain(|e| {
            e.language
                .as_ref()
                .map(|l| l.to_lowercase().contains(&lang_lower))
                .unwrap_or(false)
        });
    }

    // Apply limit
    if let Some(limit) = args.limit {
        extracted.truncate(limit);
    }

    // Output
    match output_format {
        OutputFormat::Json => {
            let json = if cli.verbose {
                serde_json::to_string_pretty(&extracted)?
            } else {
                serde_json::to_string(&extracted)?
            };
            println!("{json}");
        }
        OutputFormat::Tsv => {
            println!("index\tlanguage\tsource\ttimestamp\tcode_preview");
            for e in &extracted {
                let lang = e.language.as_deref().unwrap_or("-");
                let ts = e
                    .timestamp
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "-".to_string());
                let preview = e
                    .code
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(50)
                    .collect::<String>();
                println!("{}\t{}\t{}\t{}\t{}", e.index, lang, e.source, ts, preview);
            }
        }
        OutputFormat::Text | OutputFormat::Compact => {
            if args.concatenate {
                // Concatenate all code blocks
                for (i, e) in extracted.iter().enumerate() {
                    if i > 0 {
                        println!();
                    }
                    if args.metadata {
                        let lang = e.language.as_deref().unwrap_or("text");
                        println!("# --- Block {} ({}, {}) ---", e.index, lang, e.source);
                    }
                    println!("{}", e.code);
                }
            } else if args.files {
                // Write to individual files
                write_code_to_files(&extracted, args, &source.native_id)?;
            } else {
                // Default: show summary and code
                for e in &extracted {
                    // Use "text" for unspecified languages (more meaningful than "unknown")
                    let lang = e.language.as_deref().unwrap_or("text");
                    if args.metadata {
                        let ts = e
                            .timestamp
                            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                            .unwrap_or_else(|| "-".to_string());
                        println!("=== Block {} ===", e.index);
                        println!("Language: {}", lang);
                        println!("Source: {}", e.source);
                        println!("Timestamp: {}", ts);
                        println!("Lines: {}", e.code.lines().count());
                        println!("---");
                    } else {
                        println!("```{}", lang);
                    }
                    println!("{}", e.code);
                    if !args.metadata {
                        println!("```");
                    }
                    println!();
                }
            }

            if !cli.quiet && !args.files && !args.concatenate {
                eprintln!("\nExtracted {} code block(s)", extracted.len());
            }
        }
    }

    Ok(())
}

/// Extract code blocks from a conversation.
fn extract_code_from_conversation(
    conversation: &Conversation,
    args: &CodeArgs,
    source: &CodeSource,
) -> Vec<ExtractedCode> {
    let mut extracted = Vec::new();
    let mut index = 0;

    let entries = if args.main_thread {
        conversation.main_thread_entries()
    } else {
        conversation.chronological_entries()
    };

    for entry in entries {
        match entry {
            LogEntry::User(user) => {
                let semantically_human = !source.semantic_annotations
                    || entry
                        .uuid()
                        .and_then(|uuid| conversation.semantics_for_uuid(uuid))
                        .and_then(|semantics| semantics.prompt)
                        .is_some_and(|prompt| prompt.authorship == PromptAuthorship::Human);
                if !args.assistant_only && semantically_human {
                    let content_text = match &user.message {
                        UserContent::Simple(simple) => simple.content.clone(),
                        UserContent::Blocks(blocks) => blocks
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                ContentBlock::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };

                    for block in extract_code_blocks(&content_text) {
                        extracted.push(ExtractedCode {
                            language: block.language,
                            code: block.code,
                            source: "user".to_string(),
                            timestamp: Some(user.timestamp),
                            index,
                            provider: source.provider.clone(),
                            qualified_id: source.qualified_id.clone(),
                        });
                        index += 1;
                    }
                }
            }
            LogEntry::Assistant(assistant) => {
                if !args.user_only {
                    for content in &assistant.message.content {
                        if let ContentBlock::Text(text) = content {
                            for block in extract_code_blocks(&text.text) {
                                extracted.push(ExtractedCode {
                                    language: block.language,
                                    code: block.code,
                                    source: "assistant".to_string(),
                                    timestamp: Some(assistant.timestamp),
                                    index,
                                    provider: source.provider.clone(),
                                    qualified_id: source.qualified_id.clone(),
                                });
                                index += 1;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    extracted
}

/// Write extracted code blocks to individual files.
fn write_code_to_files(
    extracted: &[ExtractedCode],
    args: &CodeArgs,
    native_session_id: &str,
) -> Result<()> {
    use std::fs;

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Create output directory if it doesn't exist
    fs::create_dir_all(&output_dir)?;

    let session_prefix = safe_session_prefix(native_session_id);

    for e in extracted {
        let extension = language_to_extension(e.language.as_deref());
        let filename = format!("{}_block_{:03}.{}", session_prefix, e.index, extension);
        let filepath = output_dir.join(&filename);

        let mut file = fs::File::create(&filepath)?;
        writeln!(file, "{}", e.code)?;

        if !args.quiet {
            eprintln!("Wrote: {}", filepath.display());
        }
    }

    eprintln!(
        "\nWrote {} file(s) to {}",
        extracted.len(),
        output_dir.display()
    );

    Ok(())
}

/// A filesystem-safe, deterministic label for provider-native ids. Classic
/// UUIDs retain their existing eight-character prefix; hostile or Unicode ids
/// cannot inject path separators or panic at a byte boundary.
fn safe_session_prefix(native_id: &str) -> String {
    let prefix: String = native_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .take(8)
        .collect();
    if prefix.is_empty() {
        "session".to_string()
    } else {
        prefix
    }
}

/// Map language name to file extension.
fn language_to_extension(language: Option<&str>) -> &'static str {
    match language.map(|l| l.to_lowercase()).as_deref() {
        Some("rust") | Some("rs") => "rs",
        Some("python") | Some("py") => "py",
        Some("javascript") | Some("js") => "js",
        Some("typescript") | Some("ts") => "ts",
        Some("go") | Some("golang") => "go",
        Some("java") => "java",
        Some("c") => "c",
        Some("cpp") | Some("c++") => "cpp",
        Some("csharp") | Some("c#") | Some("cs") => "cs",
        Some("ruby") | Some("rb") => "rb",
        Some("php") => "php",
        Some("swift") => "swift",
        Some("kotlin") | Some("kt") => "kt",
        Some("scala") => "scala",
        Some("bash") | Some("sh") | Some("shell") => "sh",
        Some("zsh") => "zsh",
        Some("fish") => "fish",
        Some("powershell") | Some("ps1") => "ps1",
        Some("sql") => "sql",
        Some("html") => "html",
        Some("css") => "css",
        Some("scss") | Some("sass") => "scss",
        Some("less") => "less",
        Some("json") => "json",
        Some("yaml") | Some("yml") => "yaml",
        Some("toml") => "toml",
        Some("xml") => "xml",
        Some("markdown") | Some("md") => "md",
        Some("dockerfile") | Some("docker") => "dockerfile",
        Some("makefile") | Some("make") => "makefile",
        Some("lua") => "lua",
        Some("perl") | Some("pl") => "pl",
        Some("r") => "r",
        Some("julia") | Some("jl") => "jl",
        Some("elixir") | Some("ex") => "ex",
        Some("erlang") | Some("erl") => "erl",
        Some("haskell") | Some("hs") => "hs",
        Some("ocaml") | Some("ml") => "ml",
        Some("clojure") | Some("clj") => "clj",
        Some("vue") => "vue",
        Some("svelte") => "svelte",
        Some("jsx") => "jsx",
        Some("tsx") => "tsx",
        Some("graphql") | Some("gql") => "graphql",
        Some("proto") | Some("protobuf") => "proto",
        Some("terraform") | Some("tf") | Some("hcl") => "tf",
        Some("nix") => "nix",
        Some("zig") => "zig",
        Some("v") | Some("vlang") => "v",
        Some("nim") => "nim",
        Some("crystal") | Some("cr") => "cr",
        Some("dart") => "dart",
        Some("groovy") => "groovy",
        Some("assembly") | Some("asm") => "asm",
        _ => "txt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_to_extension() {
        assert_eq!(language_to_extension(Some("rust")), "rs");
        assert_eq!(language_to_extension(Some("Rust")), "rs");
        assert_eq!(language_to_extension(Some("python")), "py");
        assert_eq!(language_to_extension(Some("javascript")), "js");
        assert_eq!(language_to_extension(Some("typescript")), "ts");
        assert_eq!(language_to_extension(None), "txt");
        assert_eq!(language_to_extension(Some("unknown")), "txt");
    }

    #[test]
    fn provider_native_ids_make_safe_file_prefixes() {
        assert_eq!(safe_session_prefix("aaaaaaaa-bbbb"), "aaaaaaaa");
        assert_eq!(safe_session_prefix("../evil/session"), "evilsess");
        assert_eq!(safe_session_prefix("💥/../"), "session");
    }
}
