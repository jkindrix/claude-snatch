//! Chain command implementation.
//!
//! Lists session chains (multi-file logical sessions) for a project.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::cli::{Cli, OutputFormat};
use crate::discovery::chain::detect_chains;
use crate::error::{Result, SnatchError};
use crate::provider::{
    registry::ProviderSelection, LineageEdge, LineageEdgeKind, LogicalSessionKey,
};
use crate::util::pager::PagerWriter;

use super::get_claude_dir;

/// Arguments for the chain command.
#[derive(Debug, Clone, clap::Args)]
pub struct ChainArgs {
    /// Show typed lineage from selected providers. Omit to preserve the
    /// classic Claude continuation-chain view; use `all` for the union.
    #[arg(long = "provider", value_name = "PROVIDER")]
    pub provider: Vec<String>,

    /// Filter by project (substring match on path).
    #[arg(short, long)]
    pub project: Option<String>,
}

/// Run the chain command.
pub fn run(cli: &Cli, args: &ChainArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }
    run_classic(cli, args)
}

fn run_classic(cli: &Cli, args: &ChainArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let mut writer = PagerWriter::new(false);

    let projects = claude_dir.projects()?;
    let filtered: Vec<_> = if let Some(ref filter) = args.project {
        projects
            .into_iter()
            .filter(|p| p.best_path().contains(filter))
            .collect()
    } else {
        projects
    };

    let mut total_chains = 0;

    for project in &filtered {
        let sessions = project.main_sessions()?;
        if sessions.is_empty() {
            continue;
        }

        let chains = detect_chains(sessions.iter().map(|s| (s.session_id(), s.path())));

        if chains.is_empty() {
            continue;
        }

        // Sort chains by start time (newest first)
        let mut sorted_chains: Vec<_> = chains.values().collect();
        sorted_chains.sort_by_key(|b| std::cmp::Reverse(b.started()));

        match cli.effective_output() {
            OutputFormat::Json => {
                let output: Vec<serde_json::Value> = sorted_chains
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "root_id": c.root_id,
                            "slug": c.slug,
                            "members": c.file_ids(),
                            "length": c.len(),
                            "started": c.started().map(|t| t.to_rfc3339()),
                            "project": project.best_path(),
                        })
                    })
                    .collect();
                writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
            }
            _ => {
                writeln!(writer, "Project: {}", project.best_path())?;
                writeln!(writer, "Chains: {}", sorted_chains.len())?;
                writeln!(writer)?;

                for chain in &sorted_chains {
                    let slug_display = chain.slug.as_deref().unwrap_or("(no slug)");
                    let started = chain
                        .started()
                        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    writeln!(
                        writer,
                        "  {} [{}] ({} files, started {})",
                        &chain.root_id[..8.min(chain.root_id.len())],
                        slug_display,
                        chain.len(),
                        started,
                    )?;

                    for (i, member) in chain.members.iter().enumerate() {
                        let marker = if i == 0 { "root" } else { "cont" };
                        let ts = member
                            .started
                            .map(|t| t.format("%H:%M").to_string())
                            .unwrap_or_else(|| "??:??".to_string());
                        writeln!(
                            writer,
                            "    {}. {} ({}, {})",
                            i + 1,
                            &member.file_id[..8.min(member.file_id.len())],
                            marker,
                            ts,
                        )?;
                    }
                    writeln!(writer)?;
                }
            }
        }

        total_chains += sorted_chains.len();
    }

    if total_chains == 0 {
        writeln!(writer, "No session chains found.")?;
    }

    writer.finish()?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
struct ProviderLineageRow {
    provider: String,
    kind: &'static str,
    from: String,
    to: String,
    from_project: Option<String>,
    to_project: Option<String>,
    dangling_from: bool,
    dangling_to: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderLineageSkip {
    provider: String,
    reason: String,
}

#[derive(Debug, serde::Serialize)]
struct ProviderLineageWarning {
    qualified_id: String,
    reason: &'static str,
}

#[derive(Debug, serde::Serialize)]
struct ProviderLineageOutput {
    edges: Vec<ProviderLineageRow>,
    total_edges: usize,
    counts_by_kind: BTreeMap<&'static str, usize>,
    skipped_providers: Vec<ProviderLineageSkip>,
    warnings: Vec<ProviderLineageWarning>,
}

fn lineage_kind(kind: &LineageEdgeKind) -> &'static str {
    match kind {
        LineageEdgeKind::Continuation => "continuation",
        LineageEdgeKind::Fork => "fork",
        LineageEdgeKind::Spawn { .. } => "spawn",
    }
}

fn tsv_cell(value: &str) -> String {
    value.replace(['\t', '\r', '\n'], " ")
}

fn provider_lineage_rows(
    edges: &[LineageEdge],
    projects: &BTreeMap<LogicalSessionKey, String>,
    project_members: Option<&BTreeSet<LogicalSessionKey>>,
) -> Vec<ProviderLineageRow> {
    edges
        .iter()
        .filter(|edge| {
            project_members
                .is_none_or(|members| members.contains(&edge.from) || members.contains(&edge.to))
        })
        .map(|edge| {
            let (tool_use_id, agent_type, description) = match &edge.kind {
                LineageEdgeKind::Spawn {
                    tool_use_id,
                    agent_type,
                    description,
                } => (tool_use_id.clone(), agent_type.clone(), description.clone()),
                LineageEdgeKind::Continuation | LineageEdgeKind::Fork => (None, None, None),
            };
            ProviderLineageRow {
                provider: edge.from.provider.to_string(),
                kind: lineage_kind(&edge.kind),
                from: edge.from.to_string(),
                to: edge.to.to_string(),
                from_project: projects.get(&edge.from).cloned(),
                to_project: projects.get(&edge.to).cloned(),
                dangling_from: !projects.contains_key(&edge.from),
                dangling_to: !projects.contains_key(&edge.to),
                tool_use_id,
                agent_type,
                description,
            }
        })
        .collect()
}

fn run_provider(cli: &Cli, args: &ChainArgs) -> Result<()> {
    let ChainArgs { provider, project } = args;
    let selection =
        ProviderSelection::from_flags(provider).map_err(|reason| SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        })?;
    let registry = super::helpers::provider_registry(cli);
    let collected = registry.collect_project_union(&selection)?;

    let mut project_by_session = BTreeMap::new();
    let mut project_members = BTreeSet::new();
    for unified in &collected.projects {
        let label = unified
            .display_path
            .clone()
            .unwrap_or_else(|| unified.identity.to_string());
        let matches = project
            .as_deref()
            .is_none_or(|filter| unified.matches(filter));
        for session in &unified.sessions {
            project_by_session.insert(session.descriptor.key.clone(), label.clone());
            if matches {
                project_members.insert(session.descriptor.key.clone());
            }
        }
    }
    let rows = provider_lineage_rows(
        &collected.lineage,
        &project_by_session,
        project.as_ref().map(|_| &project_members),
    );
    let mut counts_by_kind = BTreeMap::new();
    for row in &rows {
        *counts_by_kind.entry(row.kind).or_default() += 1;
    }
    let skipped_providers = collected
        .skipped
        .into_iter()
        .map(|(provider, reason)| ProviderLineageSkip {
            provider: provider.to_string(),
            reason,
        })
        .collect::<Vec<_>>();
    let warnings = collected
        .context_warnings
        .into_iter()
        .map(|warning| ProviderLineageWarning {
            qualified_id: warning.key.to_string(),
            reason: "project metadata unavailable; lineage retained under a session fallback",
        })
        .collect::<Vec<_>>();
    let output = ProviderLineageOutput {
        total_edges: rows.len(),
        edges: rows,
        counts_by_kind,
        skipped_providers,
        warnings,
    };

    let mut writer = PagerWriter::new(false);
    match cli.effective_output() {
        OutputFormat::Json => {
            writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
        }
        OutputFormat::Tsv => {
            writeln!(
                writer,
                "provider\tkind\tfrom\tto\tfrom_project\tto_project\tdangling_from\tdangling_to\ttool_use_id\tagent_type\tdescription"
            )?;
            for row in &output.edges {
                writeln!(
                    writer,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    tsv_cell(&row.provider),
                    row.kind,
                    tsv_cell(&row.from),
                    tsv_cell(&row.to),
                    tsv_cell(row.from_project.as_deref().unwrap_or("")),
                    tsv_cell(row.to_project.as_deref().unwrap_or("")),
                    row.dangling_from,
                    row.dangling_to,
                    tsv_cell(row.tool_use_id.as_deref().unwrap_or("")),
                    tsv_cell(row.agent_type.as_deref().unwrap_or("")),
                    tsv_cell(row.description.as_deref().unwrap_or("")),
                )?;
            }
        }
        OutputFormat::Compact => {
            for row in &output.edges {
                writeln!(writer, "{}\t{}\t{}", row.kind, row.from, row.to)?;
            }
        }
        OutputFormat::Text => {
            writeln!(writer, "Session Lineage")?;
            writeln!(writer, "===============")?;
            writeln!(writer, "Edges: {}", output.total_edges)?;
            writeln!(writer)?;
            if output.edges.is_empty() {
                writeln!(writer, "No typed lineage edges found.")?;
            }
            for row in &output.edges {
                writeln!(writer, "[{}] {} -> {}", row.kind, row.from, row.to)?;
                if row.from_project == row.to_project {
                    if let Some(project) = &row.from_project {
                        writeln!(writer, "  Project: {project}")?;
                    }
                } else {
                    if let Some(project) = &row.from_project {
                        writeln!(writer, "  From project: {project}")?;
                    }
                    if let Some(project) = &row.to_project {
                        writeln!(writer, "  To project: {project}")?;
                    }
                }
                if row.dangling_from || row.dangling_to {
                    writeln!(
                        writer,
                        "  Missing endpoint inventory: {}{}",
                        if row.dangling_from { "from" } else { "" },
                        if row.dangling_to {
                            if row.dangling_from {
                                ", to"
                            } else {
                                "to"
                            }
                        } else {
                            ""
                        }
                    )?;
                }
                if let Some(tool_use_id) = &row.tool_use_id {
                    writeln!(writer, "  Spawn tool: {tool_use_id}")?;
                }
                if let Some(agent_type) = &row.agent_type {
                    writeln!(writer, "  Agent type: {agent_type}")?;
                }
                if let Some(description) = &row.description {
                    writeln!(writer, "  Description: {description}")?;
                }
                writeln!(writer)?;
            }
        }
    }
    if cli.effective_output() != OutputFormat::Json && !cli.quiet {
        for skipped in &output.skipped_providers {
            eprintln!(
                "Warning: provider '{}' was skipped: {}",
                skipped.provider, skipped.reason
            );
        }
        for warning in &output.warnings {
            eprintln!("Warning: {}: {}", warning.qualified_id, warning.reason);
        }
    }
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderId, SessionNamespace};

    fn key(native_id: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId("test".to_string()),
            namespace: SessionNamespace::global(),
            native_id: native_id.to_string(),
        }
    }

    #[test]
    fn typed_rows_preserve_kinds_spawn_metadata_and_dangling_edges() {
        let root = key("root");
        let continuation = key("continuation");
        let fork = key("fork");
        let spawn = key("spawn");
        let missing = key("missing");
        let projects = BTreeMap::from([
            (root.clone(), "/work/project".to_string()),
            (continuation.clone(), "/work/project".to_string()),
            (fork.clone(), "/work/project".to_string()),
            (spawn.clone(), "/work/project".to_string()),
        ]);
        let edges = vec![
            LineageEdge {
                from: root.clone(),
                to: continuation,
                kind: LineageEdgeKind::Continuation,
            },
            LineageEdge {
                from: missing,
                to: fork,
                kind: LineageEdgeKind::Fork,
            },
            LineageEdge {
                from: root.clone(),
                to: spawn,
                kind: LineageEdgeKind::Spawn {
                    tool_use_id: Some("tool-1".to_string()),
                    agent_type: Some("worker".to_string()),
                    description: Some("inspect".to_string()),
                },
            },
        ];

        let rows = provider_lineage_rows(&edges, &projects, None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, "continuation");
        assert_eq!(rows[1].kind, "fork");
        assert!(rows[1].dangling_from);
        assert!(!rows[1].dangling_to);
        assert_eq!(rows[2].kind, "spawn");
        assert_eq!(rows[2].tool_use_id.as_deref(), Some("tool-1"));
        assert_eq!(rows[2].agent_type.as_deref(), Some("worker"));
        assert_eq!(rows[2].description.as_deref(), Some("inspect"));
    }

    #[test]
    fn project_filter_keeps_edges_touching_a_selected_known_endpoint() {
        let selected = key("selected");
        let outside = key("outside");
        let dangling = key("dangling");
        let projects = BTreeMap::from([
            (selected.clone(), "/work/selected".to_string()),
            (outside.clone(), "/work/outside".to_string()),
        ]);
        let edges = vec![
            LineageEdge {
                from: dangling.clone(),
                to: selected.clone(),
                kind: LineageEdgeKind::Fork,
            },
            LineageEdge {
                from: outside.clone(),
                to: dangling,
                kind: LineageEdgeKind::Fork,
            },
        ];
        let members = BTreeSet::from([selected]);

        let rows = provider_lineage_rows(&edges, &projects, Some(&members));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].dangling_from);
        assert_eq!(rows[0].to_project.as_deref(), Some("/work/selected"));
    }

    #[test]
    fn tsv_cells_cannot_inject_rows_or_columns() {
        assert_eq!(tsv_cell("task\tname\r\nnext"), "task name  next");
    }
}
