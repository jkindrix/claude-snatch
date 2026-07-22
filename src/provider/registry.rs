//! Provider registry: the single seam through which provider-aware surfaces
//! (CLI, MCP, library API) obtain [`SourceProvider`] instances.
//!
//! Call sites route through the registry instead of accumulating
//! provider-specific conditionals (round-17 guardrail). Unavailable
//! providers stay visible as entries with a reason — the `--provider all`
//! partial-vs-atomic decision is made by callers, not hidden by silently
//! dropping providers here. Entries are held in [`ProviderId`] order so
//! every cross-provider iteration is deterministic.

use std::path::PathBuf;

use super::{LogicalSessionKey, ProviderError, ProviderId, SourceProvider};
use crate::discovery::ClaudeDirectory;
use crate::provider::claude_code::ClaudeCodeProvider;

/// Configuration for default provider construction: the surface's global
/// options that every provider must receive (silently dropping one is the
/// round-18 blocker-4 hazard).
#[derive(Debug, Clone, Default)]
pub struct RegistryConfig {
    /// Explicit Claude root (the CLI's global `--claude-dir`); `None`
    /// discovers.
    pub claude_root: Option<PathBuf>,
    /// Explicit Codex root for embedded/library callers; `None` discovers from
    /// Codex's normal environment/default location.
    pub codex_root: Option<PathBuf>,
    /// Global parse size limit (`--max-file-size`), in bytes.
    pub max_file_size: Option<u64>,
}

/// One installed provider: identity plus either a working instance or the
/// reason it is unavailable. The id and root stay reportable either way.
pub struct RegisteredProvider {
    /// Provider identity (present even when construction failed).
    pub id: ProviderId,
    /// Filesystem root the provider reads, when file-backed and known.
    pub root: Option<PathBuf>,
    /// The working provider, or a human-readable unavailability reason.
    pub provider: Result<Box<dyn SourceProvider>, String>,
}

/// Registry of installed providers, ordered by [`ProviderId`].
pub struct ProviderRegistry {
    entries: Vec<RegisteredProvider>,
}

impl ProviderRegistry {
    /// An empty registry (for tests and custom-root callers).
    pub fn new() -> Self {
        ProviderRegistry {
            entries: Vec::new(),
        }
    }

    /// All compiled-in providers with default discovery: `claude-code`
    /// always, `codex` when the feature is enabled. Discovery failure makes
    /// a provider unavailable, never absent.
    pub fn with_defaults() -> Self {
        Self::with_config(&RegistryConfig::default())
    }

    /// Compatibility wrapper over [`ProviderRegistry::with_config`] carrying
    /// only an explicit Claude root.
    pub fn with_claude_root(claude_root: Option<&std::path::Path>) -> Self {
        Self::with_config(&RegistryConfig {
            claude_root: claude_root.map(std::path::Path::to_path_buf),
            ..Default::default()
        })
    }

    /// Build the default providers from an explicit configuration.
    ///
    /// Every global parsing limit the surface knows about must be carried
    /// here — constructing providers without them silently loses safety
    /// options (round-18 blocker 4). `max_file_size` maps to Claude's parse
    /// size limit directly; for Codex it TIGHTENS both the compressed-input
    /// and decompressed-output caps (never loosens the defaults), making
    /// oversized sessions honest unreadable/unparseable findings. Both
    /// providers fold the limit into their parse cache tokens.
    pub fn with_config(config: &RegistryConfig) -> Self {
        let mut registry = Self::new();

        // `--max-file-size 0` means "no additional user cap" (the classic
        // CLI's zero-is-unlimited convention). Normalize it to None HERE so
        // providers keep their own built-in safety ceilings (round-19
        // blocker 2: zero must never disable Codex's bomb guards) and so
        // zero and omitted produce identical provider state — and identical
        // parse cache tokens.
        let max_file_size = config.max_file_size.filter(|&v| v != 0);

        let claude_dir = match &config.claude_root {
            Some(root) => ClaudeDirectory::from_path(root),
            None => ClaudeDirectory::discover(),
        };
        let (root, provider) = match claude_dir {
            Ok(dir) => (
                Some(dir.root().to_path_buf()),
                Ok(
                    Box::new(ClaudeCodeProvider::new(dir).with_max_file_size(max_file_size))
                        as Box<dyn SourceProvider>,
                ),
            ),
            Err(e) => (None, Err(e.to_string())),
        };
        registry
            .register(RegisteredProvider {
                id: ProviderId::claude_code(),
                root,
                provider,
            })
            .expect("empty registry cannot already contain claude-code");

        #[cfg(feature = "codex")]
        {
            let discovered = match &config.codex_root {
                Some(root) => Ok(super::codex::CodexProvider::new(root)),
                None => super::codex::CodexProvider::discover(),
            };
            let (root, provider) = match discovered {
                Ok(p) => {
                    let p = match max_file_size {
                        Some(limit) => p.tighten_limits(limit),
                        None => p,
                    };
                    let home = p.codex_home().to_path_buf();
                    if home.exists() {
                        (Some(home), Ok(Box::new(p) as Box<dyn SourceProvider>))
                    } else {
                        (Some(home), Err("codex home not found".to_string()))
                    }
                }
                Err(e) => (None, Err(e.to_string())),
            };
            registry
                .register(RegisteredProvider {
                    id: ProviderId::codex(),
                    root,
                    provider,
                })
                .expect("defaults cannot collide");
        }

        registry
    }

    /// Insert a provider entry, keeping [`ProviderId`] order. A duplicate id
    /// is an error — two instances of one provider must be modeled as
    /// namespaces, not registry entries.
    pub fn register(&mut self, entry: RegisteredProvider) -> Result<(), ProviderError> {
        match self.entries.binary_search_by(|e| e.id.cmp(&entry.id)) {
            Ok(_) => Err(ProviderError::Other(format!(
                "provider '{}' is already registered",
                entry.id
            ))),
            Err(pos) => {
                self.entries.insert(pos, entry);
                Ok(())
            }
        }
    }

    /// All entries (available or not) in deterministic id order.
    pub fn entries(&self) -> &[RegisteredProvider] {
        &self.entries
    }

    /// One entry by id, available or not.
    pub fn entry(&self, id: &ProviderId) -> Option<&RegisteredProvider> {
        self.entries
            .binary_search_by(|e| e.id.cmp(id))
            .ok()
            .map(|pos| &self.entries[pos])
    }

    /// A working provider by id, or an error naming the id and (when the
    /// provider is installed but broken) the unavailability reason. Never
    /// falls back to a different provider.
    pub fn get(&self, id: &ProviderId) -> Result<&dyn SourceProvider, ProviderError> {
        match self.entry(id) {
            None => Err(ProviderError::NotFound(format!(
                "no provider named '{id}' (known: {})",
                self.entries
                    .iter()
                    .map(|e| e.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            Some(entry) => match &entry.provider {
                Ok(p) => Ok(p.as_ref()),
                Err(reason) => Err(ProviderError::Other(format!(
                    "provider '{id}' is unavailable: {reason}"
                ))),
            },
        }
    }

    /// Working providers in deterministic id order.
    pub fn available(&self) -> impl Iterator<Item = &dyn SourceProvider> {
        self.entries
            .iter()
            .filter_map(|e| e.provider.as_ref().ok().map(|p| p.as_ref()))
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Provider selection + session resolution (the B2 resolution matrix)
// ============================================================================

/// Which providers a command operates on, from repeated `--provider` flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSelection {
    /// `--provider all`: every installed provider that is working.
    All,
    /// Explicitly named providers (deduplicated, order-independent).
    Explicit(Vec<ProviderId>),
}

impl ProviderSelection {
    /// Interpret repeated `--provider` flag values. Repeats of the same name
    /// are idempotent; mixing `all` with explicit names is an error (the
    /// intent is contradictory — `all` already includes them). Names are NOT
    /// validated here; [`ProviderRegistry::select`] does that against the
    /// installed set.
    pub fn from_flags(flags: &[String]) -> Result<Self, String> {
        let has_all = flags.iter().any(|f| f == "all");
        let explicit: Vec<ProviderId> = {
            let mut ids: Vec<ProviderId> = flags
                .iter()
                .filter(|f| *f != "all")
                .map(|f| ProviderId(f.clone()))
                .collect();
            ids.sort();
            ids.dedup();
            ids
        };
        match (has_all, explicit.is_empty()) {
            (true, false) => Err(format!(
                "--provider all cannot be combined with explicit providers ({})",
                explicit
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            (true, true) => Ok(ProviderSelection::All),
            (false, _) => Ok(ProviderSelection::Explicit(explicit)),
        }
    }
}

/// Outcome of applying a [`ProviderSelection`] to the registry.
pub struct Selected<'a> {
    /// Working providers in deterministic id order.
    pub providers: Vec<&'a dyn SourceProvider>,
    /// Providers skipped under `all` because they were unavailable
    /// (id, reason). Callers surface these as diagnostics — partial results
    /// under `all` are permitted but never silent.
    pub skipped: Vec<(ProviderId, String)>,
}

/// A resolved session reference: which provider owns it and its full key.
pub struct Resolution<'a> {
    /// The provider that owns the session.
    pub provider: &'a dyn SourceProvider,
    /// The session's complete logical key.
    pub key: LogicalSessionKey,
}

impl ProviderRegistry {
    /// Apply a selection. Availability semantics (tested, deliberate):
    /// explicitly named providers are ATOMIC — any named provider that is
    /// missing or unavailable fails the whole call; `all` is PARTIAL —
    /// unavailable providers are skipped but reported in
    /// [`Selected::skipped`]. `all` with zero working providers is an error.
    pub fn select(&self, selection: &ProviderSelection) -> Result<Selected<'_>, ProviderError> {
        match selection {
            ProviderSelection::Explicit(ids) => {
                let mut providers = Vec::with_capacity(ids.len());
                for id in ids {
                    providers.push(self.get(id)?);
                }
                Ok(Selected {
                    providers,
                    skipped: Vec::new(),
                })
            }
            ProviderSelection::All => {
                let mut providers = Vec::new();
                let mut skipped = Vec::new();
                for entry in &self.entries {
                    match &entry.provider {
                        Ok(p) => providers.push(p.as_ref()),
                        Err(reason) => skipped.push((entry.id.clone(), reason.clone())),
                    }
                }
                if providers.is_empty() {
                    return Err(ProviderError::Other(format!(
                        "no providers available: {}",
                        skipped
                            .iter()
                            .map(|(id, reason)| format!("{id}: {reason}"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    )));
                }
                Ok(Selected { providers, skipped })
            }
        }
    }

    /// Resolve a session reference against a selection.
    ///
    /// A reference containing `:` is a QUALIFIED id (the escaped
    /// [`LogicalSessionKey`] form); the named provider must be inside the
    /// selection — a qualified id never widens or overrides `--provider`,
    /// and resolution never falls back to a different provider. Anything
    /// else is an UNQUALIFIED native-id prefix searched across the selected
    /// providers: one exact native-id match wins over longer prefix matches;
    /// otherwise the match must be unique, and ambiguity is an error listing
    /// qualified candidates.
    pub fn resolve_session(
        &self,
        selection: &ProviderSelection,
        reference: &str,
    ) -> Result<Resolution<'_>, ProviderError> {
        let selected = self.select(selection)?;

        if self.looks_qualified(reference) {
            let key: LogicalSessionKey = reference
                .parse()
                .map_err(|e: String| ProviderError::Other(e))?;
            if !selected.providers.iter().any(|p| p.id() == key.provider) {
                // Precise refusal, never a fallback: an installed-but-broken
                // provider reports its reason; an installed-but-unselected
                // one points at the selection; anything else is unknown.
                return Err(match self.entry(&key.provider) {
                    Some(entry) => match &entry.provider {
                        Err(reason) => ProviderError::Other(format!(
                            "qualified id '{reference}' names provider '{}', which is \
                             unavailable: {reason}",
                            key.provider
                        )),
                        Ok(_) => ProviderError::Other(format!(
                            "qualified id '{reference}' names provider '{}', which is \
                             outside the current provider selection",
                            key.provider
                        )),
                    },
                    None => ProviderError::NotFound(format!(
                        "qualified id '{reference}' names unknown provider '{}'",
                        key.provider
                    )),
                });
            }
            // Registry order is deterministic, so this find is too.
            let provider = *selected
                .providers
                .iter()
                .find(|p| p.id() == key.provider)
                .expect("membership just checked");
            // The native-id part of a qualified reference may still be a
            // prefix; the provider is fixed, prefix rules unchanged.
            let candidates: Vec<LogicalSessionKey> = provider
                .sessions()?
                .into_iter()
                .map(|d| d.key)
                .filter(|k| k.namespace == key.namespace && k.native_id.starts_with(&key.native_id))
                .collect();
            let key = pick_unique(reference, candidates, &key.native_id)?;
            Ok(Resolution { provider, key })
        } else {
            // Unqualified resolution proves uniqueness by searching EVERY
            // selected provider. Under an explicit selection a runtime
            // sessions() failure is atomic; under `all` it makes that
            // provider UNSEARCHED — and one session found elsewhere proves
            // nothing about an unsearched provider, so any unsearched
            // provider (construction-skipped or runtime-failed) forces a
            // refusal rather than a guess (round-18 blocker 2). Qualified
            // references pin their provider and are unaffected.
            let atomic = matches!(selection, ProviderSelection::Explicit(_));
            let mut unsearched: Vec<(ProviderId, String)> = selected.skipped.clone();
            let mut candidates = Vec::new();
            let mut searched: Vec<&dyn SourceProvider> = Vec::new();
            for provider in &selected.providers {
                match provider.sessions() {
                    Ok(descriptors) => {
                        candidates.extend(
                            descriptors
                                .into_iter()
                                .map(|d| d.key)
                                .filter(|k| k.native_id.starts_with(reference)),
                        );
                        searched.push(*provider);
                    }
                    Err(e) if atomic => return Err(e),
                    Err(e) => unsearched.push((provider.id(), e.to_string())),
                }
            }
            if !unsearched.is_empty() {
                return Err(ProviderError::Other(format!(
                    "cannot resolve unqualified reference '{reference}': uniqueness is \
                     unprovable while providers were not searched ({}) — use a qualified \
                     id (provider:...) to pin the provider",
                    unsearched
                        .iter()
                        .map(|(id, reason)| format!("{id}: {reason}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                )));
            }
            let key = pick_unique(reference, candidates, reference)?;
            let provider = searched
                .iter()
                .find(|p| p.id() == key.provider)
                .copied()
                .expect("winning key came from a searched provider");
            Ok(Resolution { provider, key })
        }
    }
}

impl ProviderRegistry {
    /// Whether a CLI/MCP reference is a qualified id addressed to a
    /// REGISTERED provider (its first escaped segment names one). Used to
    /// separate qualified ids from legacy references that legitimately
    /// contain `:` (Windows paths, project filters) without ever
    /// misrouting `provider:...` to the legacy Claude path.
    pub fn looks_qualified(&self, reference: &str) -> bool {
        reference.split_once(':').is_some_and(|(first, _)| {
            !first.is_empty() && self.entry(&ProviderId(first.to_string())).is_some()
        })
    }

    /// Resolve with the compatibility default: with no `--provider` flags an
    /// UNQUALIFIED reference stays Claude-only, while a QUALIFIED id is itself
    /// an explicit provider request and resolves against exactly the provider
    /// it names. Phase D retained this default to avoid surprise scans,
    /// unavailable-provider failures, and new cross-provider ambiguity on
    /// existing commands. With flags, the full selection matrix applies. No
    /// path ever falls back to a provider the user did not name.
    pub fn resolve_with_default_policy(
        &self,
        provider_flags: &[String],
        reference: &str,
    ) -> Result<Resolution<'_>, ProviderError> {
        let selection = if !provider_flags.is_empty() {
            ProviderSelection::from_flags(provider_flags).map_err(ProviderError::Other)?
        } else if self.looks_qualified(reference) {
            let key: LogicalSessionKey = reference.parse().map_err(ProviderError::Other)?;
            ProviderSelection::Explicit(vec![key.provider])
        } else {
            ProviderSelection::Explicit(vec![ProviderId::claude_code()])
        };
        self.resolve_session(&selection, reference)
    }
}

/// One provider's diagnostics: `None` means the provider has no dedicated
/// diagnostics (a success — the classic doctor covers it).
pub type ProviderDiagnostics = (ProviderId, Option<serde_json::Value>);

/// One provider's typed lineage graph.
pub type ProviderLineage = (ProviderId, Vec<super::LineageEdge>);

/// Result of collecting across a selection with the runtime-failure
/// contract enforced centrally (round-19 blocker 4): surfaces render this,
/// they do not re-implement the semantics.
pub struct Collected<T> {
    /// Successfully collected items, in deterministic provider/key order.
    pub items: T,
    /// Providers skipped under `all` (construction- or runtime-failed),
    /// with reasons. Empty under an explicit selection (failures are
    /// atomic there).
    pub skipped: Vec<(ProviderId, String)>,
}

/// One session whose project metadata could not be read. The session remains
/// visible in a session-identity fallback project; this warning explains why
/// it could not be unified by cwd/git evidence.
#[derive(Debug, Clone)]
pub struct ProjectContextWarning {
    /// Qualified session identity.
    pub key: LogicalSessionKey,
    /// Provider error (renderers may replace it with a fixed safe message).
    pub reason: String,
}

/// Cross-provider project collection.
///
/// Uses the same partial-vs-atomic provider scan semantics as [`Collected`].
/// Project-context failure never drops a session: it produces a
/// session-identity fallback plus a warning.
pub struct CollectedProjects {
    /// Deterministically grouped projects.
    pub projects: Vec<super::project::UnifiedProject>,
    /// Providers skipped under an `all` selection.
    pub skipped: Vec<(ProviderId, String)>,
    /// Sessions retained without project evidence.
    pub context_warnings: Vec<ProjectContextWarning>,
}

/// Cross-provider projects with a complete, provider-owned lineage graph.
///
/// Consumers that collapse continuations, exclude spawns, or project fork
/// activity must use this instead of independently joining two scans.
pub struct CollectedProjectUnion {
    /// Deterministically grouped projects, excluding providers whose lineage
    /// could not be established under `all`.
    pub projects: Vec<super::project::UnifiedProject>,
    /// Sorted, deduplicated typed edges for the retained providers.
    pub lineage: Vec<super::LineageEdge>,
    /// Construction, inventory, or lineage failures softened under `all`.
    pub skipped: Vec<(ProviderId, String)>,
    /// Project-context warnings for retained sessions.
    pub context_warnings: Vec<ProjectContextWarning>,
}

/// One complete parsed bundle with the project label selected by the unified
/// project resolver.
pub struct ParsedProjectSession {
    /// Display/project path used by analysis filters and responses.
    pub project_path: String,
    /// Complete cached provider bundle.
    pub parsed: std::sync::Arc<super::ParsedSession>,
}

/// Parsed project sessions under the registry's atomic-vs-partial contract.
pub struct CollectedParsedProjectSessions {
    /// Successfully parsed sessions in deterministic project/session order.
    pub sessions: Vec<ParsedProjectSession>,
    /// Providers skipped during construction, inventory, or lineage scans.
    pub skipped: Vec<(ProviderId, String)>,
    /// Individual sessions retained by inventory but unavailable to analysis.
    pub warnings: Vec<String>,
}

/// Outcome of streaming parsed project sessions through a visitor.
pub struct ParsedProjectVisitReport {
    /// Providers skipped during construction, inventory, or lineage scans.
    pub skipped: Vec<(ProviderId, String)>,
    /// Individual sessions retained by inventory but unavailable to analysis.
    pub warnings: Vec<String>,
}

impl ProviderRegistry {
    /// Collect session descriptors across a selection.
    ///
    /// Explicit selections are atomic over runtime `sessions()` failures;
    /// `all` skips-and-reports them — but `all` with ZERO successfully
    /// scanned providers is an error, mirroring the construction-time rule.
    pub fn collect_selected_sessions(
        &self,
        selection: &ProviderSelection,
    ) -> Result<Collected<Vec<super::SessionDescriptor>>, ProviderError> {
        let selected = self.select(selection)?;
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        let mut skipped = selected.skipped.clone();
        let mut items = Vec::new();
        let mut scanned = 0usize;
        for provider in &selected.providers {
            match provider.sessions() {
                Ok(mut descriptors) => {
                    // Providers arrive in id order; keys sort within each.
                    descriptors.sort_by(|a, b| a.key.cmp(&b.key));
                    items.extend(descriptors);
                    scanned += 1;
                }
                Err(e) if atomic => return Err(e),
                Err(e) => skipped.push((provider.id(), format!("session scan failed: {e}"))),
            }
        }
        if scanned == 0 {
            return Err(no_provider_succeeded(&skipped));
        }
        Ok(Collected { items, skipped })
    }

    /// Collect provider diagnostics across a selection, same contract as
    /// [`ProviderRegistry::collect_selected_sessions`]. `None` items are
    /// providers without dedicated diagnostics (a success, not a failure).
    pub fn collect_selected_diagnostics(
        &self,
        selection: &ProviderSelection,
    ) -> Result<Collected<Vec<ProviderDiagnostics>>, ProviderError> {
        let selected = self.select(selection)?;
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        let mut skipped = selected.skipped.clone();
        let mut items = Vec::new();
        let mut succeeded = 0usize;
        for provider in &selected.providers {
            match provider.diagnostics() {
                Ok(value) => {
                    items.push((provider.id(), value));
                    succeeded += 1;
                }
                Err(e) if atomic => return Err(e),
                Err(e) => skipped.push((provider.id(), format!("diagnostics failed: {e}"))),
            }
        }
        if succeeded == 0 {
            return Err(no_provider_succeeded(&skipped));
        }
        Ok(Collected { items, skipped })
    }

    /// Collect typed lineage with the same atomic-vs-partial contract as
    /// session inventory and diagnostics. A successful empty graph is still a
    /// successful provider scan.
    pub fn collect_selected_lineage(
        &self,
        selection: &ProviderSelection,
    ) -> Result<Collected<Vec<ProviderLineage>>, ProviderError> {
        let selected = self.select(selection)?;
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        let mut skipped = selected.skipped.clone();
        let mut items = Vec::new();
        let mut succeeded = 0_usize;
        for provider in &selected.providers {
            match provider.lineage() {
                Ok(mut edges) => {
                    let provider_id = provider.id();
                    if edges.iter().any(|edge| {
                        edge.from.provider != provider_id || edge.to.provider != provider_id
                    }) {
                        let error = ProviderError::Other(format!(
                            "provider '{provider_id}' returned a lineage edge outside its own identity"
                        ));
                        if atomic {
                            return Err(error);
                        }
                        skipped.push((provider_id, format!("lineage scan failed: {error}")));
                        continue;
                    }
                    edges.sort();
                    edges.dedup();
                    items.push((provider_id, edges));
                    succeeded += 1;
                }
                Err(error) if atomic => return Err(error),
                Err(error) => {
                    skipped.push((provider.id(), format!("lineage scan failed: {error}")));
                }
            }
        }
        if succeeded == 0 {
            return Err(no_provider_succeeded(&skipped));
        }
        Ok(Collected { items, skipped })
    }

    /// Collect and group project evidence across selected providers.
    ///
    /// Provider inventory failures follow the established selection contract:
    /// explicit selections are atomic; `all` is partial-but-reported and must
    /// have at least one successful provider scan. A single unreadable
    /// session's project metadata does not erase that session from the union;
    /// it receives a session-key fallback identity and a warning.
    pub fn collect_unified_projects(
        &self,
        selection: &ProviderSelection,
    ) -> Result<CollectedProjects, ProviderError> {
        let selected = self.select(selection)?;
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        let mut skipped = selected.skipped.clone();
        let mut context_warnings = Vec::new();
        let mut project_sessions = Vec::new();
        let mut scanned = 0_usize;
        let mut local_git_cache: std::collections::HashMap<
            String,
            super::project::SessionProjectContext,
        > = std::collections::HashMap::new();

        for provider in &selected.providers {
            let mut sessions = match provider.sessions_with_project_context() {
                Ok(sessions) => sessions,
                Err(error) if atomic => return Err(error),
                Err(error) => {
                    skipped.push((provider.id(), format!("session scan failed: {error}")));
                    continue;
                }
            };
            scanned += 1;
            sessions.sort_by(|(a, _), (b, _)| a.key.cmp(&b.key));
            for (descriptor, context) in sessions {
                let mut context = match context {
                    Ok(context) => context,
                    Err(error) => {
                        context_warnings.push(ProjectContextWarning {
                            key: descriptor.key.clone(),
                            reason: error.to_string(),
                        });
                        super::project::SessionProjectContext::default()
                    }
                };

                if let Some(cwd) = context.cwd.clone() {
                    let local = local_git_cache.entry(cwd.clone()).or_insert_with(|| {
                        let mut local = super::project::SessionProjectContext {
                            cwd: Some(cwd),
                            ..Default::default()
                        };
                        super::project::enrich_from_local_git(&mut local);
                        local
                    });
                    if context.git_root.is_none() {
                        context.git_root.clone_from(&local.git_root);
                    }
                    if context.git_repository_url.is_none() {
                        context
                            .git_repository_url
                            .clone_from(&local.git_repository_url);
                    }
                    if context.git_branch.is_none() {
                        context.git_branch.clone_from(&local.git_branch);
                    }
                }
                project_sessions.push(super::project::ProjectSession {
                    descriptor,
                    context,
                });
            }
        }
        if scanned == 0 {
            return Err(no_provider_succeeded(&skipped));
        }
        context_warnings.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(CollectedProjects {
            projects: super::project::group_sessions(project_sessions),
            skipped,
            context_warnings,
        })
    }

    /// Collect unified projects plus the typed lineage required to interpret
    /// their cross-session history. Explicit selections are atomic. Under
    /// `all`, a provider whose lineage fails is removed from the project union
    /// and reported—never rendered with guessed continuation/spawn/fork state.
    pub fn collect_project_union(
        &self,
        selection: &ProviderSelection,
    ) -> Result<CollectedProjectUnion, ProviderError> {
        let mut projects = self.collect_unified_projects(selection)?;
        let lineage = self.collect_selected_lineage(selection)?;
        let successful: std::collections::BTreeSet<_> = lineage
            .items
            .iter()
            .map(|(provider, _)| provider.clone())
            .collect();
        let represented: std::collections::BTreeSet<_> = projects
            .projects
            .iter()
            .flat_map(|project| project.providers.iter().cloned())
            .collect();
        let failed: std::collections::BTreeSet<_> =
            represented.difference(&successful).cloned().collect();
        if !failed.is_empty() {
            for project in &mut projects.projects {
                project
                    .sessions
                    .retain(|session| !failed.contains(&session.descriptor.key.provider));
                project
                    .providers
                    .retain(|provider| !failed.contains(provider));
            }
            projects
                .projects
                .retain(|project| !project.sessions.is_empty());
            projects
                .context_warnings
                .retain(|warning| !failed.contains(&warning.key.provider));
        }

        projects.skipped.extend(lineage.skipped);
        projects
            .skipped
            .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        projects.skipped.dedup_by(|a, b| a.0 == b.0);
        let mut edges: Vec<_> = lineage
            .items
            .into_iter()
            .flat_map(|(_, edges)| edges)
            .collect();
        edges.sort();
        edges.dedup();
        Ok(CollectedProjectUnion {
            projects: projects.projects,
            lineage: edges,
            skipped: projects.skipped,
            context_warnings: projects.context_warnings,
        })
    }

    /// Collect complete provider bundles for analysis surfaces.
    ///
    /// The provider scan and lineage graph use [`Self::collect_project_union`].
    /// Explicit selections remain atomic when any retained session fails to
    /// parse; `all` skips that session with a qualified, non-sensitive warning.
    /// Spawned sessions are filtered only when requested, using typed lineage.
    pub fn collect_parsed_project_sessions(
        &self,
        selection: &ProviderSelection,
        cache: &crate::cache::CacheManager,
        project_filter: Option<&str>,
        include_subagents: bool,
    ) -> Result<CollectedParsedProjectSessions, ProviderError> {
        let mut sessions = Vec::new();
        let report = self.visit_parsed_project_sessions(
            selection,
            cache,
            project_filter,
            include_subagents,
            |project_path, parsed| {
                sessions.push(ParsedProjectSession {
                    project_path: project_path.to_string(),
                    parsed,
                });
            },
        )?;
        Ok(CollectedParsedProjectSessions {
            sessions,
            skipped: report.skipped,
            warnings: report.warnings,
        })
    }

    /// Stream complete parsed bundles through a callback without retaining
    /// the whole selected corpus at once.
    ///
    /// Selection, lineage, project filtering, subagent filtering, cache
    /// validation, and partial-vs-atomic error behavior are identical to
    /// [`Self::collect_parsed_project_sessions`]. Analysis surfaces whose
    /// aggregate is compact should prefer this form.
    pub fn visit_parsed_project_sessions<F>(
        &self,
        selection: &ProviderSelection,
        cache: &crate::cache::CacheManager,
        project_filter: Option<&str>,
        include_subagents: bool,
        mut visit: F,
    ) -> Result<ParsedProjectVisitReport, ProviderError>
    where
        F: FnMut(&str, std::sync::Arc<super::ParsedSession>),
    {
        let collected = self.collect_project_union(selection)?;
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        let spawned: std::collections::BTreeSet<_> = collected
            .lineage
            .iter()
            .filter(|edge| matches!(edge.kind, super::LineageEdgeKind::Spawn { .. }))
            .map(|edge| edge.to.clone())
            .collect();
        let mut warnings: Vec<String> = collected
            .context_warnings
            .iter()
            .map(|warning| format!("{}: project context unavailable", warning.key))
            .collect();
        for project in &collected.projects {
            if project_filter.is_some_and(|filter| !project.matches(filter)) {
                continue;
            }
            let project_path = project
                .display_path
                .clone()
                .unwrap_or_else(|| project.identity.to_string());
            for session in &project.sessions {
                let key = &session.descriptor.key;
                if !include_subagents && spawned.contains(key) {
                    continue;
                }
                let provider = self.get(&key.provider)?;
                match streaming_parsed_session(cache, provider, &session.descriptor) {
                    Ok(parsed) => {
                        let path = session.context.cwd.as_deref().unwrap_or(&project_path);
                        visit(path, parsed);
                    }
                    Err(error) if atomic => {
                        return Err(ProviderError::Other(format!(
                            "session {key} could not be parsed: {error}"
                        )))
                    }
                    Err(_) => warnings.push(format!("{key}: session could not be parsed")),
                }
            }
        }
        warnings.sort();
        warnings.dedup();
        Ok(ParsedProjectVisitReport {
            skipped: collected.skipped,
            warnings,
        })
    }

    /// Stream compact provider-owned file-change projections without
    /// constructing unrelated conversation entries.
    ///
    /// Selection, project/subagent filtering, and explicit-vs-`all` failure
    /// behavior match [`Self::visit_parsed_project_sessions`].
    pub fn visit_project_file_changes<F>(
        &self,
        selection: &ProviderSelection,
        project_filter: Option<&str>,
        include_subagents: bool,
        mut visit: F,
    ) -> Result<ParsedProjectVisitReport, ProviderError>
    where
        F: FnMut(&str, &super::SessionDescriptor, super::FileChangeProjection),
    {
        let collected = self.collect_project_union(selection)?;
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        let spawned: std::collections::BTreeSet<_> = collected
            .lineage
            .iter()
            .filter(|edge| matches!(edge.kind, super::LineageEdgeKind::Spawn { .. }))
            .map(|edge| edge.to.clone())
            .collect();
        let mut warnings: Vec<String> = collected
            .context_warnings
            .iter()
            .map(|warning| format!("{}: project context unavailable", warning.key))
            .collect();
        for project in &collected.projects {
            if project_filter.is_some_and(|filter| !project.matches(filter)) {
                continue;
            }
            let project_path = project
                .display_path
                .clone()
                .unwrap_or_else(|| project.identity.to_string());
            for session in &project.sessions {
                let key = &session.descriptor.key;
                if !include_subagents && spawned.contains(key) {
                    continue;
                }
                let provider = self.get(&key.provider)?;
                match provider.file_change_projection(&session.descriptor) {
                    Ok(projection) => visit(&project_path, &session.descriptor, projection),
                    Err(error) if atomic => {
                        return Err(ProviderError::Other(format!(
                            "session {key} file changes could not be read: {error}"
                        )))
                    }
                    Err(_) => {
                        warnings.push(format!("{key}: session file changes could not be read"));
                    }
                }
            }
        }
        warnings.sort();
        warnings.dedup();
        Ok(ParsedProjectVisitReport {
            skipped: collected.skipped,
            warnings,
        })
    }

    /// Parse only sessions whose compact native projection contains a
    /// non-inherited change matching `path_pattern`.
    ///
    /// This is the progressive-narrowing path for conversation-rich file
    /// evolution: cheap evidence first, full transcript only after a match.
    pub fn visit_matching_file_change_sessions<F>(
        &self,
        selection: &ProviderSelection,
        project_filter: Option<&str>,
        include_subagents: bool,
        path_pattern: &str,
        mut visit: F,
    ) -> Result<ParsedProjectVisitReport, ProviderError>
    where
        F: FnMut(&str, std::sync::Arc<super::ParsedSession>),
    {
        let mut candidates = Vec::new();
        let mut report = self.visit_project_file_changes(
            selection,
            project_filter,
            include_subagents,
            |project_path, descriptor, projection| {
                let matching = projection.changes.iter().any(|change| {
                    !projection.inherited_owners.contains(&change.owner)
                        && (change.path.contains(path_pattern)
                            || change
                                .move_path
                                .as_deref()
                                .is_some_and(|path| path.contains(path_pattern)))
                });
                if matching {
                    candidates.push((project_path.to_string(), descriptor.clone()));
                }
            },
        )?;
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        for (project_path, descriptor) in candidates {
            let provider = self.get(&descriptor.key.provider)?;
            match provider
                .parse_discovered(&descriptor)
                .map_err(crate::error::SnatchError::from)
                .and_then(|parsed| validate_provider_session(provider, parsed))
            {
                Ok(parsed) => visit(&project_path, std::sync::Arc::new(parsed)),
                Err(error) if atomic => {
                    return Err(ProviderError::Other(format!(
                        "session {} could not be parsed: {error}",
                        descriptor.key
                    )))
                }
                Err(_) => report
                    .warnings
                    .push(format!("{}: session could not be parsed", descriptor.key)),
            }
        }
        report.warnings.sort();
        report.warnings.dedup();
        Ok(report)
    }
}

fn no_provider_succeeded(skipped: &[(ProviderId, String)]) -> ProviderError {
    ProviderError::Other(format!(
        "no provider could be scanned: {}",
        skipped
            .iter()
            .map(|(id, reason)| format!("{id}: {reason}"))
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

/// Parse a provider session with caching, retaining the COMPLETE bundle.
///
/// The production consumer of [`SourceProvider::parse_cache_token`]
/// (round-11 guardrail). The full [`super::ParsedSession`] — entry ids,
/// provenance, dispositions, semantics, diagnostics — is cached under the
/// session's logical identity and revalidated against the provider's
/// current token, so an artifact revision change between lookups forces a
/// reparse. Caching only entries here made propagation illusory
/// (round-18); the bundle is the canonical parsed representation.
pub fn cached_parsed_session(
    cache: &crate::cache::CacheManager,
    provider: &dyn SourceProvider,
    key: &LogicalSessionKey,
) -> crate::error::Result<std::sync::Arc<super::ParsedSession>> {
    let token = provider.parse_cache_token(key)?;
    cache.get_or_parse_provider_session(key, &token, || {
        parse_and_validate_provider_session(provider, key)
    })
}

/// Parse one bundle for a streaming corpus scan without inserting cache
/// misses. A warm, revision-matching cache entry is still reused.
fn streaming_parsed_session(
    cache: &crate::cache::CacheManager,
    provider: &dyn SourceProvider,
    descriptor: &super::SessionDescriptor,
) -> crate::error::Result<std::sync::Arc<super::ParsedSession>> {
    let token = provider.parse_cache_token_for_descriptor(descriptor)?;
    if let Some(parsed) = cache.get_provider_session_if_cached(&descriptor.key, &token) {
        return Ok(parsed);
    }
    let parsed = provider.parse_discovered(descriptor)?;
    validate_provider_session(provider, parsed).map(std::sync::Arc::new)
}

fn parse_and_validate_provider_session(
    provider: &dyn SourceProvider,
    key: &LogicalSessionKey,
) -> crate::error::Result<super::ParsedSession> {
    validate_provider_session(provider, provider.parse(key)?)
}

fn validate_provider_session(
    provider: &dyn SourceProvider,
    parsed: super::ParsedSession,
) -> crate::error::Result<super::ParsedSession> {
    let violations = parsed.validate_provenance();
    if !violations.is_empty() {
        return Err(ProviderError::Other(format!(
            "provider '{}' returned invalid normalized provenance ({} violation{})",
            provider.id(),
            violations.len(),
            if violations.len() == 1 { "" } else { "s" }
        ))
        .into());
    }
    Ok(parsed)
}

/// Uniqueness rule shared by qualified-prefix and unqualified resolution:
/// exactly one EXACT native-id match wins outright; otherwise the candidate
/// set must have exactly one member. Ambiguity errors list qualified ids
/// (capped) so the user can retry unambiguously.
fn pick_unique(
    reference: &str,
    candidates: Vec<LogicalSessionKey>,
    native_prefix: &str,
) -> Result<LogicalSessionKey, ProviderError> {
    let exact: Vec<&LogicalSessionKey> = candidates
        .iter()
        .filter(|k| k.native_id == native_prefix)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    match candidates.len() {
        1 => Ok(candidates.into_iter().next().expect("len checked")),
        0 => {
            let mut msg = format!("no session matching '{reference}'");
            if reference.contains(':') {
                msg.push_str(
                    " (colon-bearing references are treated as qualified ids only when \
                     the first segment names a registered provider)",
                );
            }
            Err(ProviderError::NotFound(msg))
        }
        n => {
            const SHOW: usize = 5;
            // Sort BEFORE truncating so the listed candidates are the
            // lexicographically first five, deterministically (round-18).
            let mut shown: Vec<String> = candidates.iter().map(ToString::to_string).collect();
            shown.sort();
            shown.truncate(SHOW);
            let more = if n > SHOW {
                format!(" and {} more", n - SHOW)
            } else {
                String::new()
            };
            Err(ProviderError::Other(format!(
                "'{reference}' is ambiguous: {n} sessions match ({}{more}) — use a longer \
                 prefix or a qualified id",
                shown.join(", ")
            )))
        }
    }
}
