//! Recent command implementation.
//!
//! A shorthand for `list -n 5` to quickly show recent sessions.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Local, Utc};

use crate::cli::{Cli, OutputFormat, RecentArgs};
use crate::discovery::Session;
use crate::error::Result;
use crate::provider::{
    project::UnifiedProject, registry::ProviderSelection, LineageEdge, LineageEdgeKind,
    LogicalSessionKey,
};
use crate::tags::TagStore;
use crate::util::truncate_path;

use super::get_claude_dir;

/// Session info for JSON output.
#[derive(Debug, serde::Serialize)]
struct SessionInfo {
    id: String,
    project: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    modified: DateTime<Utc>,
    size_bytes: u64,
    entry_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bookmarked: Option<bool>,
}

impl SessionInfo {
    fn from_session(session: &Session, tag_store: &TagStore) -> Self {
        let id = session.session_id().to_string();
        let tags = tag_store.get(&id);
        let entry_count = session.quick_metadata_cached().ok().map(|m| m.entry_count);
        Self {
            id: id.clone(),
            project: session.display_project_path(),
            modified: session.modified_datetime(),
            size_bytes: session.file_size(),
            entry_count,
            name: tags.and_then(|t| t.name.clone()),
            bookmarked: tags.map(|t| t.bookmarked),
        }
    }
}

#[derive(Debug, Clone)]
struct ProviderRecentMember {
    key: LogicalSessionKey,
    project_key: String,
    project: String,
    modified: Option<DateTime<Utc>>,
    size_bytes: u64,
}

#[derive(Debug, serde::Serialize)]
struct ProviderRecentRow {
    provider: String,
    qualified_id: String,
    native_id: String,
    latest_qualified_id: String,
    continuation_member_count: usize,
    continuation_members: Vec<String>,
    project_key: String,
    project: String,
    modified: Option<DateTime<Utc>>,
    size_bytes: u64,
}

#[derive(Debug, serde::Serialize)]
struct ProviderRecentSkip {
    provider: String,
    reason: String,
}

#[derive(Debug, serde::Serialize)]
struct ProviderRecentWarning {
    qualified_id: String,
    reason: &'static str,
}

#[derive(Debug, serde::Serialize)]
struct ProviderRecentOutput {
    sessions: Vec<ProviderRecentRow>,
    total: usize,
    skipped_providers: Vec<ProviderRecentSkip>,
    warnings: Vec<ProviderRecentWarning>,
}

/// Provider-routed recent inventory.
///
/// This deliberately stays descriptor-only: recent-session discovery must not
/// parse every transcript merely to obtain an entry count. Continuations are
/// collapsed from typed lineage, while forks and spawned sessions remain
/// independent rows. Tags are omitted until their store uses qualified keys.
fn run_provider(cli: &Cli, args: &RecentArgs) -> Result<()> {
    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        crate::error::SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let registry = super::helpers::provider_registry(cli);

    // A flat descriptor view does not need lineage and therefore must not
    // fail merely because a provider's lineage capability is unavailable.
    let (mut projects, lineage, skipped, warnings) = if args.no_chain {
        let collected = registry.collect_unified_projects(&selection)?;
        (
            collected.projects,
            Vec::new(),
            collected.skipped,
            collected.context_warnings,
        )
    } else {
        let collected = registry.collect_project_union(&selection)?;
        (
            collected.projects,
            collected.lineage,
            collected.skipped,
            collected.context_warnings,
        )
    };

    if let Some(filter) = args.project.as_deref() {
        projects.retain(|project| project.matches(filter));
    }

    let mut rows = provider_recent_rows(&projects, &lineage, args.no_chain);
    rows.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.qualified_id.cmp(&b.qualified_id))
    });
    let total = rows.len();
    rows.truncate(args.count);

    let skipped_providers = skipped
        .iter()
        .map(|(provider, reason)| ProviderRecentSkip {
            provider: provider.to_string(),
            reason: reason.clone(),
        })
        .collect();
    let warning_rows = warnings
        .iter()
        .map(|warning| ProviderRecentWarning {
            qualified_id: warning.key.to_string(),
            reason: "project metadata unavailable; session retained in its own project",
        })
        .collect();

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = ProviderRecentOutput {
                sessions: rows,
                total,
                skipped_providers,
                warnings: warning_rows,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!(
                "qualified_id\tproject\tmodified\tsize\tcontinuation_members\tlatest_qualified_id"
            );
            for row in &rows {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    row.qualified_id,
                    row.project,
                    row.modified
                        .map_or_else(String::new, |time| time.to_rfc3339()),
                    row.size_bytes,
                    row.continuation_member_count,
                    row.latest_qualified_id,
                );
            }
        }
        OutputFormat::Compact => {
            for row in &rows {
                let project = truncate_path(&row.project, 40);
                if row.continuation_member_count > 1 {
                    println!(
                        "{} {} (continuation: {}, latest {})",
                        row.qualified_id,
                        project,
                        row.continuation_member_count,
                        row.latest_qualified_id,
                    );
                } else {
                    println!("{} {project}", row.qualified_id);
                }
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                if !cli.quiet {
                    println!("No recent sessions found.");
                }
            } else {
                println!("Recent Sessions");
                println!("{}", "=".repeat(80));
                println!();
                for row in &rows {
                    let time = row.modified.map_or_else(
                        || "unknown".to_string(),
                        |value| {
                            value
                                .with_timezone(&Local)
                                .format("%Y-%m-%d %H:%M")
                                .to_string()
                        },
                    );
                    let project = truncate_path(&row.project, 45);
                    let detail = if row.continuation_member_count > 1 {
                        format!("{} continuation members", row.continuation_member_count)
                    } else {
                        String::new()
                    };
                    println!(
                        "  {} │ {:45} │ {} │ {}",
                        row.qualified_id, project, time, detail
                    );
                }
                println!();
                println!("Tip: Use 'snatch info <qualified-id>' for details.");
            }
        }
    }

    for (provider, reason) in skipped {
        eprintln!("warning: provider '{provider}' skipped: {reason}");
    }
    if !warnings.is_empty() {
        eprintln!(
            "warning: {} session(s) lacked project metadata and were retained separately",
            warnings.len()
        );
    }
    Ok(())
}

fn provider_recent_rows(
    projects: &[UnifiedProject],
    lineage: &[LineageEdge],
    no_chain: bool,
) -> Vec<ProviderRecentRow> {
    let mut rows = Vec::new();
    for project in projects {
        let project_key = project.identity.to_string();
        let project_path = project
            .display_path
            .clone()
            .unwrap_or_else(|| project_key.clone());
        let members: BTreeMap<_, _> = project
            .sessions
            .iter()
            .map(|session| {
                let key = session.descriptor.key.clone();
                (
                    key.clone(),
                    ProviderRecentMember {
                        key,
                        project_key: project_key.clone(),
                        project: project_path.clone(),
                        modified: session.context.modified_at,
                        size_bytes: session.context.artifact_bytes,
                    },
                )
            })
            .collect();

        if no_chain {
            rows.extend(
                members
                    .values()
                    .cloned()
                    .map(|member| provider_recent_row(vec![member])),
            );
            continue;
        }

        let keys: BTreeSet<_> = members.keys().cloned().collect();
        let spawned: BTreeSet<_> = lineage
            .iter()
            .filter_map(|edge| match edge.kind {
                LineageEdgeKind::Spawn { .. } if keys.contains(&edge.to) => Some(edge.to.clone()),
                _ => None,
            })
            .collect();
        let mut parents = BTreeMap::new();
        for edge in lineage {
            if edge.kind != LineageEdgeKind::Continuation
                || !keys.contains(&edge.from)
                || !keys.contains(&edge.to)
            {
                continue;
            }
            parents
                .entry(edge.to.clone())
                .and_modify(|parent: &mut LogicalSessionKey| {
                    if edge.from < *parent {
                        parent.clone_from(&edge.from);
                    }
                })
                .or_insert_with(|| edge.from.clone());
        }
        let mut groups: BTreeMap<LogicalSessionKey, Vec<(usize, ProviderRecentMember)>> =
            BTreeMap::new();
        for member in members.values() {
            let (root, depth) = continuation_root(&member.key, &parents, &spawned);
            groups
                .entry(root)
                .or_default()
                .push((depth, member.clone()));
        }
        for (_, mut grouped) in groups {
            grouped.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.key.cmp(&b.1.key)));
            rows.push(provider_recent_row(
                grouped.into_iter().map(|(_, member)| member).collect(),
            ));
        }
    }
    rows
}

fn continuation_root(
    key: &LogicalSessionKey,
    parents: &BTreeMap<LogicalSessionKey, LogicalSessionKey>,
    spawned: &BTreeSet<LogicalSessionKey>,
) -> (LogicalSessionKey, usize) {
    if spawned.contains(key) {
        return (key.clone(), 0);
    }
    let mut current = key.clone();
    let mut path = Vec::new();
    loop {
        if let Some(cycle_start) = path.iter().position(|seen| seen == &current) {
            let root = path[cycle_start..]
                .iter()
                .min()
                .cloned()
                .expect("cycle contains current key");
            return (root, 0);
        }
        path.push(current.clone());
        let Some(parent) = parents.get(&current) else {
            return (current, path.len().saturating_sub(1));
        };
        // Spawned sessions are independent recent rows, even if malformed or
        // future lineage also describes them as continuation endpoints.
        if spawned.contains(parent) {
            return (key.clone(), 0);
        }
        current = parent.clone();
    }
}

fn provider_recent_row(members: Vec<ProviderRecentMember>) -> ProviderRecentRow {
    let root = members.first().expect("recent group is non-empty");
    let latest = members
        .iter()
        .max_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.key.cmp(&b.key)))
        .expect("recent group is non-empty");
    ProviderRecentRow {
        provider: root.key.provider.to_string(),
        qualified_id: root.key.to_string(),
        native_id: root.key.native_id.clone(),
        latest_qualified_id: latest.key.to_string(),
        continuation_member_count: members.len(),
        continuation_members: members
            .iter()
            .map(|member| member.key.to_string())
            .collect(),
        project_key: root.project_key.clone(),
        project: root.project.clone(),
        modified: latest.modified,
        size_bytes: members.iter().fold(0_u64, |total, member| {
            total.saturating_add(member.size_bytes)
        }),
    }
}

/// Run the recent command.
///
/// By default, resume chains are collapsed into one logical-conversation row
/// keyed by the chain root. `--no-chain` restores the flat per-file view.
pub fn run(cli: &Cli, args: &RecentArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let tag_store = TagStore::load()?;

    // Get all sessions
    let mut sessions = claude_dir.all_sessions()?;

    // Filter by project if specified
    if let Some(project_filter) = &args.project {
        let projects = claude_dir.projects()?;
        let matched = super::helpers::filter_projects(projects, project_filter);
        let matched_paths: Vec<String> = matched
            .iter()
            .map(|p| p.decoded_path().to_string())
            .collect();
        sessions.retain(|s| {
            matched_paths
                .iter()
                .any(|mp| s.project_path().contains(mp.as_str()))
        });
    }

    if !args.no_chain {
        return run_collapsed(cli, args, sessions, &tag_store);
    }

    // Sessions are already sorted by modification time (most recent first)
    // Take the requested count
    sessions.truncate(args.count);

    if sessions.is_empty() {
        if !cli.quiet {
            println!("No recent sessions found.");
        }
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = sessions
                .iter()
                .map(|s| SessionInfo::from_session(s, &tag_store))
                .collect();
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("id\tproject\tmodified\tsize\tname");
            for session in &sessions {
                let id = session.session_id();
                let tags = tag_store.get(id);
                let name = tags
                    .and_then(|t| t.name.as_ref())
                    .map(|n| n.as_str())
                    .unwrap_or("");
                let project = session.display_project_path();
                let modified = session
                    .modified_datetime()
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    &id[..8.min(id.len())],
                    project,
                    modified,
                    session.file_size(),
                    name
                );
            }
        }
        OutputFormat::Compact => {
            for session in &sessions {
                let id = session.session_id();
                let short_id = &id[..8.min(id.len())];
                let project = session.display_project_path();
                let display_project = truncate_path(&project, 40);
                println!("{} {}", short_id, display_project);
            }
        }
        OutputFormat::Text => {
            println!("Recent Sessions");
            println!("{}", "=".repeat(60));
            println!();

            for session in &sessions {
                print_session_line(session, &tag_store)?;
            }

            println!();
            println!("Tip: Use 'snatch info <id>' for details or 'snatch pick' to browse.");
        }
    }

    Ok(())
}

/// Logical-conversation row for JSON output (chains collapsed).
#[derive(Debug, serde::Serialize)]
struct LogicalSessionInfo {
    id: String,
    latest_session_id: String,
    chain_member_count: usize,
    chain_members: Vec<String>,
    project: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    modified: DateTime<Utc>,
    size_bytes: u64,
    entry_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bookmarked: Option<bool>,
}

impl LogicalSessionInfo {
    fn from_row(row: &super::helpers::LogicalSession, tag_store: &TagStore) -> Self {
        let root = row.root();
        let tags = tag_store.get(&row.root_id);
        // Sum entry counts across members for the logical conversation.
        let mut entry_count = None;
        for s in &row.members {
            if let Ok(m) = s.quick_metadata_cached() {
                entry_count = Some(entry_count.unwrap_or(0) + m.entry_count);
            }
        }
        Self {
            id: row.root_id.clone(),
            latest_session_id: row.latest_session_id().to_string(),
            chain_member_count: row.member_count(),
            chain_members: row.member_ids(),
            project: root.display_project_path(),
            modified: DateTime::<Utc>::from(row.latest_modified()),
            size_bytes: row.total_size(),
            entry_count,
            name: tags.and_then(|t| t.name.clone()),
            bookmarked: tags.map(|t| t.bookmarked),
        }
    }
}

/// Run the recent command with resume chains collapsed into logical rows.
fn run_collapsed(
    cli: &Cli,
    args: &RecentArgs,
    sessions: Vec<Session>,
    tag_store: &TagStore,
) -> Result<()> {
    let mut rows = super::helpers::group_into_logical(sessions);
    rows.sort_by_key(|r| std::cmp::Reverse(r.latest_modified()));
    rows.truncate(args.count);

    if rows.is_empty() {
        if !cli.quiet {
            println!("No recent sessions found.");
        }
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = rows
                .iter()
                .map(|r| LogicalSessionInfo::from_row(r, tag_store))
                .collect();
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("id\tproject\tmodified\tsize\tname\tmember_count\tlatest_session_id");
            for row in &rows {
                let id = &row.root_id;
                let tags = tag_store.get(id);
                let name = tags
                    .and_then(|t| t.name.as_ref())
                    .map(|n| n.as_str())
                    .unwrap_or("");
                let project = row.root().display_project_path();
                let modified = DateTime::<Utc>::from(row.latest_modified())
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    &id[..8.min(id.len())],
                    project,
                    modified,
                    row.total_size(),
                    name,
                    row.member_count(),
                    &row.latest_session_id()[..8.min(row.latest_session_id().len())],
                );
            }
        }
        OutputFormat::Compact => {
            for row in &rows {
                let id = &row.root_id;
                let short_id = &id[..8.min(id.len())];
                let project = row.root().display_project_path();
                let display_project = truncate_path(&project, 40);
                if row.is_chain() {
                    println!(
                        "{} {} (chain: {}, latest {})",
                        short_id,
                        display_project,
                        row.member_count(),
                        &row.latest_session_id()[..8.min(row.latest_session_id().len())],
                    );
                } else {
                    println!("{} {}", short_id, display_project);
                }
            }
        }
        OutputFormat::Text => {
            println!("Recent Sessions");
            println!("{}", "=".repeat(60));
            println!();

            for row in &rows {
                print_logical_line(row, tag_store)?;
            }

            println!();
            println!("Tip: Use 'snatch info <id>' for details or 'snatch pick' to browse.");
        }
    }

    Ok(())
}

/// Print a formatted logical-conversation line (chains collapsed).
fn print_logical_line(row: &super::helpers::LogicalSession, tag_store: &TagStore) -> Result<()> {
    let root = row.root();
    let id = &row.root_id;
    let short_id = &id[..8.min(id.len())];
    let tags = tag_store.get(id);

    let mut indicators = String::new();
    if let Some(t) = tags {
        if t.bookmarked {
            indicators.push_str("★ ");
        }
    }

    let name_or_project = tags
        .and_then(|t| t.name.clone())
        .unwrap_or_else(|| root.display_project_path());
    let display_name = truncate_path(&name_or_project, 45);

    let time_str = DateTime::<Utc>::from(row.latest_modified())
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();

    let info = if row.is_chain() {
        format!("chain: {} files", row.member_count())
    } else {
        row.root()
            .quick_metadata_cached()
            .ok()
            .map(|m| format!("{} entries", m.entry_count))
            .unwrap_or_default()
    };

    println!(
        "  {}{} │ {:45} │ {} │ {}",
        indicators, short_id, display_name, time_str, info
    );

    Ok(())
}

/// Print a formatted session line.
fn print_session_line(session: &Session, tag_store: &TagStore) -> Result<()> {
    let id = session.session_id();
    let short_id = &id[..8.min(id.len())];
    let tags = tag_store.get(id);

    // Build status indicators
    let mut indicators = String::new();
    if let Some(t) = tags {
        if t.bookmarked {
            indicators.push_str("★ ");
        }
    }

    // Session name or project
    let name_or_project = tags
        .and_then(|t| t.name.clone())
        .unwrap_or_else(|| session.display_project_path());

    // Truncate if too long (use consistent 45 char limit for text mode)
    let display_name = truncate_path(&name_or_project, 45);

    // Modification time
    let time_str = session
        .modified_datetime()
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();

    // Entry count (quick metadata)
    let entry_info = session
        .quick_metadata_cached()
        .ok()
        .map(|m| format!("{} entries", m.entry_count))
        .unwrap_or_default();

    println!(
        "  {}{} │ {:45} │ {} │ {}",
        indicators, short_id, display_name, time_str, entry_info
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        project::{ProjectIdentity, ProjectIdentityBasis, ProjectSession, SessionProjectContext},
        ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot, ProviderId, SessionArtifact,
        SessionDescriptor, SessionNamespace,
    };

    fn key(id: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId("test".into()),
            namespace: SessionNamespace::global(),
            native_id: id.into(),
        }
    }

    fn project_session(id: &str, size: u64) -> ProjectSession {
        let key = key(id);
        ProjectSession {
            descriptor: SessionDescriptor {
                key: key.clone(),
                artifacts: vec![SessionArtifact {
                    snapshot: ArtifactSnapshot {
                        id: ArtifactId {
                            provider_instance: "fixture".into(),
                            locator: id.into(),
                        },
                        revision: ArtifactRevision("r1".into()),
                    },
                    form: ArtifactForm::Database,
                    archived: false,
                }],
            },
            context: SessionProjectContext {
                cwd: Some("/repo".into()),
                artifact_bytes: size,
                ..Default::default()
            },
        }
    }

    #[test]
    fn provider_recent_collapses_only_continuations() {
        let project = UnifiedProject {
            identity: ProjectIdentity {
                basis: ProjectIdentityBasis::Cwd,
                value: "/repo".into(),
            },
            display_path: Some("/repo".into()),
            cwd_variants: vec!["/repo".into()],
            git_repository: None,
            providers: vec![ProviderId("test".into())],
            sessions: vec![
                project_session("root", 1),
                project_session("continued", 2),
                project_session("fork", 4),
                project_session("spawn", 8),
            ],
        };
        let lineage = vec![
            LineageEdge {
                from: key("root"),
                to: key("continued"),
                kind: LineageEdgeKind::Continuation,
            },
            LineageEdge {
                from: key("root"),
                to: key("fork"),
                kind: LineageEdgeKind::Fork,
            },
            LineageEdge {
                from: key("root"),
                to: key("spawn"),
                kind: LineageEdgeKind::Spawn {
                    tool_use_id: None,
                    agent_type: None,
                    description: None,
                },
            },
        ];

        let rows = provider_recent_rows(std::slice::from_ref(&project), &lineage, false);
        assert_eq!(rows.len(), 3);
        let root = rows
            .iter()
            .find(|row| row.native_id == "root")
            .expect("continuation root");
        assert_eq!(root.continuation_member_count, 2);
        assert_eq!(root.size_bytes, 3);
        assert!(rows
            .iter()
            .any(|row| { row.native_id == "fork" && row.continuation_member_count == 1 }));
        assert!(rows
            .iter()
            .any(|row| { row.native_id == "spawn" && row.continuation_member_count == 1 }));

        let flat = provider_recent_rows(&[project], &lineage, true);
        assert_eq!(flat.len(), 4);
        assert!(flat.iter().all(|row| row.continuation_member_count == 1));
    }
}
