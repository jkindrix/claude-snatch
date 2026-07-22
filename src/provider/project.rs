//! Provider-neutral project identity and cross-provider grouping.
//!
//! Session ids identify conversations; projects identify the working context
//! those conversations belong to. Providers expose native evidence (cwd and
//! git metadata) and this module groups it deterministically without treating
//! a display path as globally unique.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::Path;

use chrono::{DateTime, Utc};

use super::{
    LineageEdge, LineageEdgeKind, LogicalSessionKey, ParsedSession, ProviderId, SessionDescriptor,
};
use crate::model::LogEntry;

/// Native and locally-derived evidence tying one session to a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionProjectContext {
    /// Working directory recorded by the session.
    pub cwd: Option<String>,
    /// Git repository root, when the working directory is still available.
    pub git_root: Option<String>,
    /// Credential-free repository identity (normally the origin URL).
    pub git_repository_url: Option<String>,
    /// Branch recorded by the session or current local repository.
    pub git_branch: Option<String>,
    /// First native event time, when cheaply available.
    pub started_at: Option<DateTime<Utc>>,
    /// Last native event time, when cheaply available.
    pub ended_at: Option<DateTime<Utc>>,
    /// The physical tail could not be resolved as a complete timestamped
    /// native record. Aggregate period filters may conservatively consult
    /// source modification time even when an earlier complete event exists.
    pub native_tail_unresolved: bool,
    /// Source-artifact modification time.
    pub modified_at: Option<DateTime<Utc>>,
    /// Preferred source-artifact size.
    pub artifact_bytes: u64,
}

impl SessionProjectContext {
    /// Derive context from a complete normalized bundle. Provider adapters
    /// override the trait method with cheaper native metadata reads when they
    /// can; this is the content-complete fallback.
    #[must_use]
    pub fn from_parsed(parsed: &ParsedSession) -> Self {
        let mut out = Self::default();
        for identified in &parsed.entries {
            let entry = &identified.entry;
            out.cwd
                .get_or_insert_with(|| entry.cwd().unwrap_or_default().to_string());
            if out.cwd.as_deref() == Some("") {
                out.cwd = None;
            }
            out.git_branch
                .get_or_insert_with(|| entry.git_branch().unwrap_or_default().to_string());
            if out.git_branch.as_deref() == Some("") {
                out.git_branch = None;
            }
            if let Some(timestamp) = entry.timestamp() {
                out.started_at.get_or_insert(timestamp);
                out.ended_at = Some(timestamp);
            }
            if let LogEntry::Unknown(value) = entry {
                let payload = value.get("payload").unwrap_or(value);
                if out.cwd.is_none() {
                    out.cwd = payload
                        .get("cwd")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                }
                if let Some(git) = payload.get("git") {
                    if out.git_repository_url.is_none() {
                        out.git_repository_url = git
                            .get("repository_url")
                            .or_else(|| git.get("repositoryUrl"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                    }
                    if out.git_branch.is_none() {
                        out.git_branch = git
                            .get("branch")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                    }
                }
            }
        }
        out
    }
}

/// Evidence basis selected for a stable project identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectIdentityBasis {
    /// Credential-free normalized git remote.
    GitRemote,
    /// Canonical local git repository root.
    GitRoot,
    /// Normalized recorded working directory.
    Cwd,
    /// No project evidence was available; session identity is the fallback.
    Session,
}

impl ProjectIdentityBasis {
    /// Stable machine-readable label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitRemote => "git-remote",
            Self::GitRoot => "git-root",
            Self::Cwd => "cwd",
            Self::Session => "session",
        }
    }
}

/// Provider-neutral project identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectIdentity {
    /// Evidence used to form the key.
    pub basis: ProjectIdentityBasis,
    /// Normalized, credential-free value.
    pub value: String,
}

impl fmt::Display for ProjectIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.basis.as_str(), self.value)
    }
}

/// One provider session assigned to a unified project.
#[derive(Debug, Clone)]
pub struct ProjectSession {
    /// Logical session and artifacts.
    pub descriptor: SessionDescriptor,
    /// Project evidence used for grouping and presentation.
    pub context: SessionProjectContext,
}

/// Cross-provider project group.
#[derive(Debug, Clone)]
pub struct UnifiedProject {
    /// Deterministic identity.
    pub identity: ProjectIdentity,
    /// Preferred human-readable cwd (frequency, then length, then lexical).
    pub display_path: Option<String>,
    /// Every recorded cwd represented in the group.
    pub cwd_variants: Vec<String>,
    /// Normalized git remote when it is the grouping basis.
    pub git_repository: Option<String>,
    /// Providers represented by sessions in this project.
    pub providers: Vec<ProviderId>,
    /// Sessions in qualified-key order.
    pub sessions: Vec<ProjectSession>,
}

/// One cross-session history unit. Continuation members collapse into one
/// logical conversation; forks remain independent; spawned subagents are
/// excluded from the project-level main-session history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHistoryUnit {
    /// Root session shown to callers.
    pub root: LogicalSessionKey,
    /// Continuation members in parent-depth/key order (root first).
    pub members: Vec<LogicalSessionKey>,
}

impl UnifiedProject {
    /// Latest source modification represented in this project.
    #[must_use]
    pub fn latest_modified(&self) -> Option<DateTime<Utc>> {
        self.sessions
            .iter()
            .filter_map(|session| session.context.modified_at)
            .max()
    }

    /// Sum of preferred-artifact sizes (saturating).
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.sessions.iter().fold(0_u64, |sum, session| {
            sum.saturating_add(session.context.artifact_bytes)
        })
    }

    /// Whether a user-facing substring matches identity, path, repository, or
    /// any cwd variant.
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        self.identity.to_string().contains(needle)
            || self
                .display_path
                .as_deref()
                .is_some_and(|path| path.contains(needle))
            || self
                .git_repository
                .as_deref()
                .is_some_and(|repo| repo.contains(needle))
            || self.cwd_variants.iter().any(|cwd| cwd.contains(needle))
    }
}

/// Normalize a cwd for equality without requiring it to still exist.
/// Windows drive paths are slash-normalized and drive-case-folded; other
/// paths retain case. `.` and lexical `..` segments are collapsed.
#[must_use]
pub fn normalize_cwd(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let windows_drive = trimmed.as_bytes().get(1) == Some(&b':');
    let normalized = if windows_drive || trimmed.contains('\\') {
        trimmed.replace('\\', "/")
    } else {
        trimmed.to_string()
    };
    let mut prefix = String::new();
    let mut rest = normalized.as_str();
    if windows_drive {
        let drive = normalized[..1].to_ascii_lowercase();
        prefix = format!("{drive}:");
        rest = &normalized[2..];
    } else if normalized.starts_with('/') {
        prefix.push('/');
        rest = normalized.trim_start_matches('/');
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if prefix.is_empty() => parts.push(part),
            ".." => {}
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    let value = if prefix == "/" {
        format!("/{joined}")
    } else if prefix.ends_with(':') {
        if joined.is_empty() {
            format!("{prefix}/")
        } else {
            format!("{prefix}/{joined}")
        }
    } else {
        joined
    };
    (!value.is_empty()).then_some(value)
}

/// Normalize a git remote into a credential-free equality key.
#[must_use]
pub fn normalize_git_remote(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/').trim_end_matches(".git");
    if raw.is_empty() {
        return None;
    }

    if let Some((_, authority_and_path)) = raw.split_once("://") {
        let (authority, path) = authority_and_path
            .split_once('/')
            .unwrap_or((authority_and_path, ""));
        let host = authority
            .rsplit('@')
            .next()
            .unwrap_or(authority)
            .to_ascii_lowercase();
        let path = path.trim_matches('/');
        return Some(if path.is_empty() {
            host
        } else {
            format!("{host}/{path}")
        });
    }

    // SCP-like SSH syntax: git@host:owner/repo.
    if let Some((authority, path)) = raw.split_once(':') {
        if authority.contains('@') && !authority.contains('/') {
            let host = authority
                .rsplit('@')
                .next()
                .unwrap_or(authority)
                .to_ascii_lowercase();
            return Some(format!("{host}/{}", path.trim_matches('/')));
        }
    }

    normalize_cwd(raw).map(|path| format!("local/{path}"))
}

/// Fill missing git evidence from a cwd that still exists. Callers cache this
/// by cwd so a project scan does not shell out once per session.
pub fn enrich_from_local_git(context: &mut SessionProjectContext) {
    let Some(cwd) = context.cwd.as_deref() else {
        return;
    };
    let Some(repo) = crate::git::get_repo_info(Path::new(cwd)) else {
        return;
    };
    context.git_root.get_or_insert(repo.root);
    if let Some(remote) = repo.remote_url {
        context.git_repository_url.get_or_insert(remote);
    }
    if let Some(branch) = repo.branch {
        context.git_branch.get_or_insert(branch);
    }
}

/// Group provider sessions by repository first, then local git root, then cwd.
///
/// A cwd reused for two distinct remotes is deliberately NOT a bridge between
/// them. Sessions lacking git metadata join a remote group through cwd only
/// when that cwd identifies exactly one remote; otherwise they remain in a
/// separate cwd group. This prevents transitive accidental merges.
#[must_use]
pub fn group_sessions(sessions: Vec<ProjectSession>) -> Vec<UnifiedProject> {
    let mut normalized_remote = Vec::with_capacity(sessions.len());
    let mut normalized_root = Vec::with_capacity(sessions.len());
    let mut normalized_path = Vec::with_capacity(sessions.len());
    let mut cwd_remotes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for session in &sessions {
        let remote = session
            .context
            .git_repository_url
            .as_deref()
            .and_then(normalize_git_remote);
        let root = session.context.git_root.as_deref().and_then(normalize_cwd);
        let cwd = session.context.cwd.as_deref().and_then(normalize_cwd);
        if let (Some(cwd), Some(remote)) = (&cwd, &remote) {
            cwd_remotes
                .entry(cwd.clone())
                .or_default()
                .insert(remote.clone());
        }
        normalized_remote.push(remote);
        normalized_root.push(root);
        normalized_path.push(cwd);
    }

    let mut groups: BTreeMap<ProjectIdentity, Vec<ProjectSession>> = BTreeMap::new();
    for (index, session) in sessions.into_iter().enumerate() {
        let identity = if let Some(remote) = normalized_remote[index].clone() {
            ProjectIdentity {
                basis: ProjectIdentityBasis::GitRemote,
                value: remote,
            }
        } else if let Some(root) = normalized_root[index].clone() {
            ProjectIdentity {
                basis: ProjectIdentityBasis::GitRoot,
                value: root,
            }
        } else if let Some(cwd) = normalized_path[index].clone() {
            match cwd_remotes.get(&cwd) {
                Some(remotes) if remotes.len() == 1 => ProjectIdentity {
                    basis: ProjectIdentityBasis::GitRemote,
                    value: remotes.iter().next().expect("len checked").clone(),
                },
                _ => ProjectIdentity {
                    basis: ProjectIdentityBasis::Cwd,
                    value: cwd,
                },
            }
        } else {
            ProjectIdentity {
                basis: ProjectIdentityBasis::Session,
                value: session.descriptor.key.to_string(),
            }
        };
        groups.entry(identity).or_default().push(session);
    }

    groups
        .into_iter()
        .map(|(identity, mut sessions)| {
            sessions.sort_by(|a, b| a.descriptor.key.cmp(&b.descriptor.key));
            let mut cwd_counts: HashMap<String, usize> = HashMap::new();
            let mut providers = BTreeSet::new();
            for session in &sessions {
                providers.insert(session.descriptor.key.provider.clone());
                if let Some(cwd) = &session.context.cwd {
                    *cwd_counts.entry(cwd.clone()).or_default() += 1;
                }
            }
            let mut cwd_variants: Vec<String> = cwd_counts.keys().cloned().collect();
            cwd_variants.sort();
            let display_path = cwd_counts
                .into_iter()
                .max_by(|(a_path, a_count), (b_path, b_count)| {
                    a_count
                        .cmp(b_count)
                        .then_with(|| b_path.len().cmp(&a_path.len()))
                        .then_with(|| b_path.cmp(a_path))
                })
                .map(|(path, _)| path);
            let git_repository =
                (identity.basis == ProjectIdentityBasis::GitRemote).then(|| identity.value.clone());
            UnifiedProject {
                identity,
                display_path,
                cwd_variants,
                git_repository,
                providers: providers.into_iter().collect(),
                sessions,
            }
        })
        .collect()
}

/// Build logical history units from typed lineage.
///
/// Only `Continuation` collapses sessions. `Fork` must remain visible as its
/// own work stream (with inherited entries removed later), while `Spawn`
/// identifies subagent sessions omitted from the main project history.
#[must_use]
pub fn history_units(project: &UnifiedProject, lineage: &[LineageEdge]) -> Vec<ProjectHistoryUnit> {
    let keys: BTreeSet<LogicalSessionKey> = project
        .sessions
        .iter()
        .map(|session| session.descriptor.key.clone())
        .collect();
    let mut continuation_parent: BTreeMap<LogicalSessionKey, LogicalSessionKey> = BTreeMap::new();
    let mut spawned = BTreeSet::new();
    for edge in lineage {
        if !keys.contains(&edge.to) {
            continue;
        }
        match edge.kind {
            LineageEdgeKind::Continuation if keys.contains(&edge.from) => {
                continuation_parent
                    .entry(edge.to.clone())
                    .and_modify(|parent| {
                        if edge.from < *parent {
                            parent.clone_from(&edge.from);
                        }
                    })
                    .or_insert_with(|| edge.from.clone());
            }
            LineageEdgeKind::Spawn { .. } => {
                spawned.insert(edge.to.clone());
            }
            LineageEdgeKind::Continuation | LineageEdgeKind::Fork => {}
        }
    }

    fn root_and_depth(
        key: &LogicalSessionKey,
        parents: &BTreeMap<LogicalSessionKey, LogicalSessionKey>,
    ) -> (LogicalSessionKey, usize) {
        let mut current = key.clone();
        let mut path = Vec::new();
        loop {
            if let Some(cycle_start) = path.iter().position(|seen| seen == &current) {
                // Malformed provider lineage must still group deterministically
                // rather than splitting one cycle differently by starting key.
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
            current = parent.clone();
        }
    }

    let mut grouped: BTreeMap<LogicalSessionKey, Vec<(usize, LogicalSessionKey)>> = BTreeMap::new();
    for key in keys {
        if spawned.contains(&key) {
            continue;
        }
        let (root, depth) = root_and_depth(&key, &continuation_parent);
        grouped.entry(root).or_default().push((depth, key));
    }
    grouped
        .into_iter()
        .map(|(root, mut members)| {
            members.sort();
            ProjectHistoryUnit {
                root,
                members: members.into_iter().map(|(_, key)| key).collect(),
            }
        })
        .collect()
}

/// A session's provider id, for renderers that do not need the full key.
#[must_use]
pub fn session_provider(session: &ProjectSession) -> &ProviderId {
    &session.descriptor.key.provider
}

/// Copy one parsed entry list while excluding fork-inherited history.
///
/// This is the cross-session "new work" projection required by acceptance
/// invariant #4; single-session views intentionally retain inherited history.
#[must_use]
pub fn new_activity_entries(parsed: &ParsedSession) -> Vec<crate::model::LogEntry> {
    parsed
        .entries
        .iter()
        .filter(|entry| {
            parsed.semantics.get(&entry.id).map_or(true, |semantics| {
                semantics.activity == super::ActivityKind::New
            })
        })
        .map(|entry| entry.entry.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot, LogicalSessionKey,
        ProviderId, SessionArtifact, SessionNamespace,
    };

    fn session(provider: &str, id: &str, cwd: &str, remote: Option<&str>) -> ProjectSession {
        let key = LogicalSessionKey {
            provider: ProviderId(provider.into()),
            namespace: SessionNamespace::global(),
            native_id: id.into(),
        };
        ProjectSession {
            descriptor: SessionDescriptor {
                key,
                artifacts: vec![SessionArtifact {
                    snapshot: ArtifactSnapshot {
                        id: ArtifactId {
                            provider_instance: provider.into(),
                            locator: id.into(),
                        },
                        revision: ArtifactRevision("r1".into()),
                    },
                    form: ArtifactForm::PlainFile,
                    archived: false,
                }],
            },
            context: SessionProjectContext {
                cwd: Some(cwd.into()),
                git_repository_url: remote.map(str::to_string),
                ..Default::default()
            },
        }
    }

    #[test]
    fn same_cwd_without_git_unifies_across_providers() {
        let projects = group_sessions(vec![
            session("claude-code", "a", "/work/app", None),
            session("codex", "b", "/work/app/.", None),
        ]);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].sessions.len(), 2);
        assert_eq!(projects[0].identity.basis, ProjectIdentityBasis::Cwd);
    }

    #[test]
    fn same_remote_unifies_different_worktrees_and_strips_credentials() {
        let projects = group_sessions(vec![
            session(
                "claude-code",
                "a",
                "/work/one",
                Some("https://secret@example.com/acme/app.git"),
            ),
            session(
                "codex",
                "b",
                "/work/two",
                Some("git@example.com:acme/app.git"),
            ),
        ]);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].identity.value, "example.com/acme/app");
        assert!(!projects[0].identity.to_string().contains("secret"));
    }

    #[test]
    fn reused_cwd_with_conflicting_remotes_never_bridges_projects() {
        let projects = group_sessions(vec![
            session("claude-code", "a", "/work/app", Some("ssh://h/a.git")),
            session("codex", "b", "/work/app", Some("ssh://h/b.git")),
            session("codex", "c", "/work/app", None),
        ]);
        assert_eq!(projects.len(), 3);
        assert_eq!(
            projects
                .iter()
                .filter(|project| project.identity.basis == ProjectIdentityBasis::GitRemote)
                .count(),
            2
        );
        assert!(projects
            .iter()
            .any(|project| project.identity.basis == ProjectIdentityBasis::Cwd));
    }

    #[test]
    fn windows_paths_normalize_without_case_folding_the_project_path() {
        assert_eq!(
            normalize_cwd(r"C:\\Users\\Me\\src\\.\\app\\..\\tool\\"),
            Some("c:/Users/Me/src/tool".into())
        );
    }

    #[test]
    fn history_collapses_only_continuations_and_omits_spawned_sessions() {
        let sessions = vec![
            session("claude-code", "root", "/work/app", None),
            session("claude-code", "resume", "/work/app", None),
            session("codex", "fork", "/work/app", None),
            session("codex", "subagent", "/work/app", None),
        ];
        let project = group_sessions(sessions).pop().unwrap();
        let key = |provider: &str, native: &str| LogicalSessionKey {
            provider: ProviderId(provider.into()),
            namespace: SessionNamespace::global(),
            native_id: native.into(),
        };
        let lineage = vec![
            LineageEdge {
                from: key("claude-code", "root"),
                to: key("claude-code", "resume"),
                kind: LineageEdgeKind::Continuation,
            },
            LineageEdge {
                from: key("claude-code", "root"),
                to: key("codex", "fork"),
                kind: LineageEdgeKind::Fork,
            },
            LineageEdge {
                from: key("codex", "fork"),
                to: key("codex", "subagent"),
                kind: LineageEdgeKind::Spawn {
                    tool_use_id: None,
                    agent_type: None,
                    description: None,
                },
            },
        ];
        let units = history_units(&project, &lineage);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].members.len(), 2);
        assert_eq!(units[1].root.native_id, "fork");
        assert!(units
            .iter()
            .flat_map(|unit| &unit.members)
            .all(|key| key.native_id != "subagent"));
    }

    #[test]
    fn malformed_continuation_cycle_groups_deterministically() {
        let project = group_sessions(vec![
            session("claude-code", "a", "/work/app", None),
            session("claude-code", "b", "/work/app", None),
        ])
        .pop()
        .unwrap();
        let key = |native: &str| LogicalSessionKey {
            provider: ProviderId::claude_code(),
            namespace: SessionNamespace::global(),
            native_id: native.into(),
        };
        let mut edges = vec![
            LineageEdge {
                from: key("a"),
                to: key("b"),
                kind: LineageEdgeKind::Continuation,
            },
            LineageEdge {
                from: key("b"),
                to: key("a"),
                kind: LineageEdgeKind::Continuation,
            },
        ];
        let forward = history_units(&project, &edges);
        edges.reverse();
        let reverse = history_units(&project, &edges);
        assert_eq!(forward, reverse);
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].root, key("a"));
        assert_eq!(forward[0].members, vec![key("a"), key("b")]);
    }

    #[test]
    fn cross_session_projection_excludes_inherited_entries_without_hiding_new_work() {
        use crate::provider::fake::{multi_artifact_key, FakeProvider};
        use crate::provider::SourceProvider;

        let parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();
        let inherited = parsed
            .entries
            .iter()
            .filter(|entry| {
                parsed.semantics.get(&entry.id).is_some_and(|semantics| {
                    semantics.activity == crate::provider::ActivityKind::InheritedHistory
                })
            })
            .count();
        assert_eq!(inherited, 1, "fixture must bite");
        let projected = new_activity_entries(&parsed);
        assert_eq!(projected.len(), parsed.entries.len() - inherited);
        assert!(projected
            .iter()
            .any(|entry| entry.message_type() == "assistant"));
    }
}
