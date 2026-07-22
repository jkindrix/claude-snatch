# Export Formats

snatch separates normalized presentation formats from source-fidelity tiers.
Choose the contract first: readable/filterable output, exact logical records,
one exact artifact, or a lossless multi-artifact bundle.

## Format matrix

| Format | Typical extension | Contract |
|--------|-------------------|----------|
| `markdown` / `md` | `.md` | Readable normalized transcript |
| `json` | `.json` | Normalized structured conversation |
| `json-pretty` | `.json` | Indented normalized JSON |
| `text` | `.txt` | Plain normalized transcript |
| `csv` | `.csv` | Tabular normalized entries |
| `html` | `.html` | Self-contained rendered transcript |
| `sqlite` | `.db` | Queryable normalized relational export |
| `jsonl` | `.jsonl` | Normalized line-delimited representation |
| `raw-jsonl` | `.jsonl` | Exact logical JSONL record stream |
| `native` | provider-defined | Exact bytes of the preferred artifact |
| `archive` | `.bundle` | Lossless manifest plus every artifact |

Normalized output is universal. `archive` is the universal lossless tier.
`native` and `raw-jsonl` are provider capabilities; `snatch providers` reports
their availability.

## Basic usage

```bash
snatch export <SESSION>                              # Markdown to stdout
snatch export <SESSION> --format markdown -O out.md
snatch export <SESSION> --format json-pretty -O out.json
snatch export <SESSION> --format html --toc --dark -O out.html
snatch export <SESSION> --format sqlite -O out.db
```

Provider-qualified ids route directly:

```bash
snatch export codex:<SESSION> -f markdown -O conversation.md
snatch export claude-code:<SESSION> -f json -O conversation.json
```

Use `-O` or `--out` for a destination path. Global `-o` / `--output` selects
the CLI response encoding and is not the export destination option.

## Normalized formats

Normalized exporters consume the common `Conversation` model. They retain
successfully parsed content but are not byte-exact: fields can be reordered,
UUID-less entries can move relative to the tree view, and provider-routed
JSONL adds versioned derivation/provenance wrappers.

### Markdown and text

```bash
snatch export <SESSION> -f markdown -O conversation.md
snatch export <SESSION> -f text -O conversation.txt
```

Markdown includes readable role sections, fenced code, tool activity, token
statistics, and conversation/activity timing when available. It distinguishes
the native record span from authoritative model turn duration instead of
presenting trailing housekeeping activity as model latency.

### JSON and JSONL

```bash
snatch export <SESSION> -f json -O conversation.json
snatch export <SESSION> -f json-pretty -O conversation.json
snatch export <SESSION> -f jsonl -O normalized.jsonl
```

Use JSON for one structured document and JSONL for line-oriented normalized
processing. Neither is the original source stream.

### HTML

```bash
snatch export <SESSION> -f html -O conversation.html
snatch export <SESSION> -f html --dark --toc -O conversation.html
```

HTML is self-contained and supports syntax highlighting, collapsible sections,
light/dark themes, and an optional table of contents.

### CSV

```bash
snatch export <SESSION> -f csv -O conversation.csv
```

CSV is intended for entry-level spreadsheet processing. Rich nested content is
flattened or summarized; choose JSON/SQLite for lossless normalized structure.

### SQLite

```bash
snatch export <SESSION> -f sqlite -O conversation.db
sqlite3 conversation.db 'SELECT type, COUNT(*) FROM entries GROUP BY type'
```

The database includes session metadata, entries, content blocks, tool
uses/results, thinking blocks, usage statistics, and full-text indexes.

## Source-fidelity tiers

### `raw-jsonl`

```bash
snatch export codex:<SESSION> -f raw-jsonl -O rollout.jsonl
```

Streams the unmodified logical JSONL record sequence. A compressed source is
decoded because the contract is the JSONL stream, not its container bytes.

### `native`

```bash
snatch export codex:<SESSION> -f native -O preferred-artifact.bin
```

Streams the exact bytes of the provider-selected preferred artifact. For a
compressed twin, those bytes remain compressed. This tier does not include
sibling artifacts.

### `archive`

```bash
snatch export codex:<SESSION> -f archive -O session.bundle
```

Writes a provider-defined lossless bundle with a manifest and every discovered
artifact for the logical session. Use this tier when independent recovery of
all native records matters.

Source-fidelity tiers bypass parsing transformations, content filters,
redaction, templates, and presentation options. Incompatible combinations are
rejected before the destination is replaced.

## Content controls

Thinking, tool use, tool results, images, timestamps, and usage are enabled by
default.

```bash
snatch export <SESSION> --no-thinking
snatch export <SESSION> --no-tool-use --no-tool-results
snatch export <SESSION> --no-images
snatch export <SESSION> --system --metadata
snatch export <SESSION> --main-thread
snatch export <SESSION> --no-timestamps --no-usage
```

Exclusive filtering uses `--only`:

```bash
snatch export <SESSION> --only prompts
snatch export <SESSION> --only prompts,assistant
snatch export <SESSION> --only tool-use,tool-results
snatch export <SESSION> --only code
```

`--full` enables the content-complete normalized preset: system entries,
metadata, thinking, tool use, and tool results. The deprecated `--lossless`
alias means the same normalized preset; it does not promise byte fidelity.

## Privacy and redaction

```bash
# Mark what would be redacted without removing it
snatch export <SESSION> --redact security --redact-preview

# Redact credentials and security-sensitive values
snatch export <SESSION> --redact security -O safe.md

# Also redact emails, IP addresses, and phone numbers
snatch export <SESSION> --redact all -O safe.md

# Report possible PII without changing output
snatch export <SESSION> --warn-pii
```

`--only` is a focus filter, not a privacy boundary. Use `--redact` when output
will leave the trusted environment. Redaction is intentionally unavailable on
source-fidelity tiers because altering bytes would violate their contract.

## Chains and subagents

```bash
# Classic single-session export reconstructs continuation chains by default
snatch export <SESSION> -O conversation.md

# Restrict to the resolved file
snatch export <SESSION> --no-chain -O one-file.md

# Interleave Claude parent and subagent transcripts
snatch export <SESSION> --combine-agents -O combined.md
```

`--subagents` affects `--all` batch discovery. `--combine-agents` applies to a
single Claude parent export. Provider lineage and multi-artifact fidelity use
the provider seam rather than the classic subagent directory mechanism.

## Destinations

```bash
snatch export <SESSION> -O output.md
snatch export <SESSION> --clipboard
snatch export <SESSION> --gist
snatch export <SESSION> --gist --gist-public
snatch export <SESSION> --template my-template
snatch export --list-templates
```

File destinations are written through a temporary file and renamed after a
successful provider export. Use `--overwrite` when replacing existing output.

## Batch export

```bash
mkdir -p ./exports

snatch export --all \
  --project /path/to/project \
  --since 2026-07-01 \
  --format markdown \
  --out ./exports/ \
  --progress

snatch export --all \
  --provider codex \
  --format archive \
  --out ./archives/ \
  --overwrite
```

`--provider all` is an explicit full-union scan. Prefer one provider plus
project/date filters for interactive work on large corpora.

## Programmatic normalized export

```rust,no_run
use claude_snatch::export::{ExportOptions, Exporter, MarkdownExporter};
use claude_snatch::reconstruction::Conversation;

fn write_markdown(
    conversation: &Conversation,
    mut output: impl std::io::Write,
) -> claude_snatch::Result<()> {
    let exporter = MarkdownExporter::new();
    let options = ExportOptions::default()
        .with_thinking(true)
        .with_tool_use(true);
    exporter.export_conversation(conversation, &mut output, &options)
}
```

Provider ingestion should construct the conversation through
`Conversation::from_parsed_session` so entry ids, provenance, semantics, and
diagnostics survive into consumers.
