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

use super::{ProviderError, ProviderId, SourceProvider};
use crate::discovery::ClaudeDirectory;
use crate::provider::claude_code::ClaudeCodeProvider;

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
        Self::with_claude_root(None)
    }

    /// Like [`ProviderRegistry::with_defaults`], but with an explicit Claude
    /// root (the CLI's global `--claude-dir`) instead of discovery.
    pub fn with_claude_root(claude_root: Option<&std::path::Path>) -> Self {
        let mut registry = Self::new();

        let claude_dir = match claude_root {
            Some(root) => ClaudeDirectory::from_path(root),
            None => ClaudeDirectory::discover(),
        };
        let (root, provider) = match claude_dir {
            Ok(dir) => (
                Some(dir.root().to_path_buf()),
                Ok(Box::new(ClaudeCodeProvider::new(dir)) as Box<dyn SourceProvider>),
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
            let (root, provider) = match super::codex::CodexProvider::discover() {
                Ok(p) => {
                    let home = p.codex_home().to_path_buf();
                    if home.exists() {
                        (Some(home), Ok(Box::new(p) as Box<dyn SourceProvider>))
                    } else {
                        (
                            Some(home.clone()),
                            Err(format!("codex home not found at {}", home.display())),
                        )
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
