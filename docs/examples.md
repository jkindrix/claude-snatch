# Examples and Recipes

These examples use the current CLI. Run `snatch <command> --help` for the
complete option set.

## Orient first

Start with provider availability and narrow before reading message bodies:

```bash
snatch providers
snatch list projects --provider codex
snatch list sessions --provider codex -p /path/to/project -n 20
snatch info codex:<session-id>
snatch timeline codex:<session-id> -l 20
snatch messages codex:<session-id> --detail conversation -l 20
```

Flagless unqualified commands keep the classic Claude-only behavior:

```bash
snatch list sessions -n 20
snatch info <claude-session-prefix>
snatch messages <claude-session-prefix> --detail standard
```

## Provider selection

### One provider

```bash
snatch list sessions --provider claude-code
snatch list sessions --provider codex
snatch doctor --provider codex --all
snatch chain --provider codex
```

### Explicit union

```bash
snatch list projects --provider all
snatch list sessions --provider all -p /path/to/project --since 7days
snatch search "schema drift" --provider all -p /path/to/project
snatch lessons --all --provider all -p /path/to/project
```

`--provider all` scans every selected corpus and can be expensive. Prefer a
single provider and add project/date/session filters before requesting a full
union. Repeating explicit provider flags is equivalent to an explicit subset:

```bash
snatch list sessions \
  --provider claude-code \
  --provider codex \
  --project /path/to/project
```

### Qualified ids and ambiguity

Provider listings emit qualified ids such as `claude-code:<uuid>` and
`codex:<thread-id>`. A qualified id routes without a redundant flag:

```bash
snatch info codex:<thread-id>
snatch digest claude-code:<session-id>
snatch tag name codex:<thread-id> "Parser investigation"
```

An unqualified prefix is resolved only within the selected/default scope and
must be unique. If snatch reports ambiguity, copy a qualified id from `list`.

## Progressive session reading

```bash
# Prompt boundaries and compact turn summaries
snatch chunks codex:<session-id>
snatch timeline codex:<session-id>

# Conversation text without tool-only turns
snatch messages codex:<session-id> --detail conversation

# One prompt-boundary chunk, with tool details
snatch messages codex:<session-id> --chunk 4 --detail full

# Failed tool activity only
snatch messages codex:<session-id> --chunk 2-5 --detail full --errors-only
```

This orientation → timeline/chunks → selected message detail workflow avoids
placing an entire large transcript in the terminal or an agent context.

## Search and topic threading

```bash
# Classic direct search
snatch search "authentication" -p /path/to/project

# Provider-aware indexed search
snatch index build --provider codex
snatch search "authentication" --provider codex -p /path/to/project

# Build and search a complete provider union
snatch index rebuild --provider all
snatch search "migration" --provider all --since 30days

# Chronological cross-session narrative
snatch thread "retry policy" --provider all -p /path/to/project
```

Plain ASCII literals can use the index accelerator. Complex regex and fuzzy
queries may require stored-projection scans; narrow them first on large
snapshots.

## Exporting

### Normalized formats

```bash
snatch export codex:<session-id> -f markdown -O conversation.md
snatch export codex:<session-id> -f json-pretty -O conversation.json
snatch export codex:<session-id> -f html --toc --dark -O conversation.html
snatch export codex:<session-id> -f sqlite -O conversation.db
```

Normalized exports support filters and redaction:

```bash
snatch export <session-id> --only prompts,assistant -O readable.md
snatch export <session-id> --no-thinking --no-tool-results -O compact.md
snatch export <session-id> --redact security -O sanitized.md
snatch export <session-id> --redact security --redact-preview
```

### Fidelity tiers

```bash
# Normalized JSONL: content-preserving, not byte-exact
snatch export codex:<session-id> -f jsonl -O normalized.jsonl

# Exact logical JSONL record stream (compressed sources are decoded)
snatch export codex:<session-id> -f raw-jsonl -O rollout.jsonl

# Exact bytes of the preferred artifact (possibly compressed)
snatch export codex:<session-id> -f native -O preferred-artifact.bin

# Lossless manifest + every artifact of the logical session
snatch export codex:<session-id> -f archive -O session.bundle
```

Source-fidelity tiers intentionally do not apply content filters or redaction.
Use a normalized format when transforming or sanitizing content.

### Batch archive

```bash
mkdir -p ./exports
snatch export --all \
  --provider codex \
  --project /path/to/project \
  --since 30days \
  --format archive \
  --out ./exports/ \
  --overwrite \
  --progress
```

Use `--provider all` only when the batch really should include every provider.

## Analysis

```bash
snatch stats codex:<session-id> --all
snatch digest codex:<session-id>
snatch lessons codex:<session-id>
snatch health /path/to/project --provider all --since 30days
snatch priorities /path/to/project --provider all --since 30days
snatch file-history src/main.rs --provider all -p /path/to/project
snatch file-evolution src/main.rs /path/to/project --provider all
```

Provider pricing is explicit. Sources that cannot be honestly priced report
cost as unavailable rather than `$0` or an approximate fallback rate.

## Session metadata

Tags, names, bookmarks, outcomes, notes, and links use qualified logical keys:

```bash
snatch tag add investigation -s codex:<session-id>
snatch tag name codex:<session-id> "Investigate parser drift"
snatch tag bookmark codex:<session-id>
snatch tag outcome codex:<session-id> success
snatch tag note codex:<session-id> "Validated against the native records"
snatch tag link codex:<session-id> claude-code:<session-id>

snatch list sessions --provider all --tag investigation
snatch list sessions --provider all --bookmarked
snatch info codex:<session-id>
```

Bulk date/project tagging and similarity remain Claude-only until a typed
cross-provider contract exists; unsupported combinations refuse rather than
silently ignoring provider scope.

## Lineage and integrity

```bash
# Classic continuation chains
snatch chain

# Typed continuation/fork/spawn graph
snatch chain --provider all

# One normalized bundle
snatch validate codex:<session-id>

# Every selected provider
snatch validate --provider all --all

# Native vocabulary and coverage diagnostics
snatch doctor --provider codex --all
```

Preserved unknown records are content-complete data, not automatic corruption.
Use `doctor` for vocabulary drift and `validate` for source/provenance
integrity.

## Recover files from a Claude session

`recover` (alias `restore`) reconstructs files from Claude Write/Edit tool
operations. It is not a session-backup command and currently refuses other
providers.

```bash
snatch recover <session-id> --preview
snatch recover <session-id> --apply-edits --file "src/**/*.rs" --preview
snatch recover <session-id> \
  --apply-edits \
  --strip-prefix /home/user/project \
  --output-dir ./recovered \
  --overwrite
```

## MCP server

Install the current checkout, then restart/reconnect the MCP client:

```bash
./install.sh
# Equivalent explicit command:
cargo install --path . --locked --all-features --force

snatch serve-mcp
```

Example stdio configuration:

```json
{
  "mcpServers": {
    "snatch": {
      "command": "snatch",
      "args": ["serve-mcp"]
    }
  }
}
```

Replacing the binary does not restart an existing stdio subprocess.

## Library provider discovery

```rust,no_run
use claude_snatch::provider::registry::{ProviderRegistry, ProviderSelection};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = ProviderRegistry::with_defaults();
    let selection = ProviderSelection::from_flags(&["all".to_string()])
        .expect("valid provider selection");
    let collected = registry.collect_selected_sessions(&selection)?;

    for descriptor in collected.items {
        println!("{}", descriptor.key);
    }
    for (provider, reason) in collected.skipped {
        eprintln!("skipped {provider}: {reason}");
    }
    Ok(())
}
```

Explicit selections fail atomically. `all` can soften provider failures, but
the skipped reasons remain part of the result.

## Troubleshooting

### Provider or session unavailable

```bash
snatch providers --json
snatch list sessions --provider codex --full-ids
snatch info codex:<session-id> --json
```

Check `CODEX_HOME` for Codex or `SNATCH_CLAUDE_DIR`/`--claude-dir` for Claude.
Pre-envelope Codex files are discoverable and source-exportable but
intentionally refused for normalized analysis.

### Parse size refusal

```bash
snatch info codex:<session-id> --max-file-size 104857600
```

A nonzero value tightens provider guards. `0` means no additional user cap;
it never disables built-in compressed-stream protections.

### Installed binary still behaves like an old build

```bash
which -a snatch
./install.sh --local
```

Ensure the intended install directory wins in `PATH`, then restart/reconnect
the MCP client so it launches a new server process.
