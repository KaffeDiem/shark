//! Extensible boundary between coding tools and Shark's usage UI.
//!
//! Implement [`UsageProvider`] and register it in [`Registry`] for another tool. IDs are
//! stable persistence keys, independent of labels and list order. A provider owns its
//! incremental cache; callers run its blocking methods on a background executor.
//! The built-ins deliberately retain Shark's proven CLI account-query protocols.
use super::{Allowance, Failure, Provider, activity::Daily};
use std::sync::{Mutex, OnceLock};

pub trait UsageProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    /// Official remaining allowance, never inferred from token consumption.
    /// Providers without quota reporting return `Failure::Unsupported`.
    fn remaining_usage(&self) -> Result<Vec<Allowance>, Failure>;
    /// Tokens per local day, oldest first. No prompts or credentials cross this boundary.
    fn activity(&self, now: f64) -> Daily;
    fn supports_custom_roots(&self) -> bool {
        false
    }
    /// Called on the UI thread; implementations must not wait for an active scan.
    /// Returns whether configuration changed. Parsing/cache invalidation happens in workers.
    fn configure_scan_roots(&self, _raw: &str) -> bool {
        false
    }
    /// Full usage accounting, kept distinct from the existing navigation heatmap.
    fn observations(&self, now: f64) -> Observation {
        let activity = self.activity(now);
        Observation {
            report: super::logs::Report::from_daily(activity.clone()),
            activity,
        }
    }
}

pub struct Observation {
    pub activity: Daily,
    pub report: super::logs::Report,
}

/// A registered source and its process-local UI slot. Persistence always uses `id`,
/// never the slot, so future sources do not need variants in the built-in parser enum.
#[derive(Clone, Copy)]
pub struct ProviderHandle {
    index: usize,
    source: &'static dyn UsageProvider,
}
impl ProviderHandle {
    pub fn index(self) -> usize {
        self.index
    }
    pub fn source(self) -> &'static dyn UsageProvider {
        self.source
    }
    pub fn id(self) -> &'static str {
        self.source.id()
    }
    pub fn name(self) -> &'static str {
        self.source.name()
    }
}
impl From<Provider> for ProviderHandle {
    fn from(provider: Provider) -> Self {
        builtins()
            .handles()
            .find(|handle| handle.id() == provider.source().id())
            .expect("built-in usage provider registered")
    }
}

#[derive(Default)]
pub struct Registry {
    sources: Vec<Box<dyn UsageProvider>>,
}

impl Registry {
    /// Reject invalid or duplicate IDs instead of silently replacing a provider's history.
    pub fn register(&mut self, source: impl UsageProvider + 'static) -> Result<(), String> {
        let id = source.id();
        if id.is_empty()
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        {
            return Err(
                "Usage provider IDs must contain lowercase ASCII letters, digits, '-' or '_'"
                    .into(),
            );
        }
        if self.sources.iter().any(|source| source.id() == id) {
            return Err(format!("Usage provider '{id}' is already registered"));
        }
        self.sources.push(Box::new(source));
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&dyn UsageProvider> {
        self.sources
            .iter()
            .find(|source| source.id() == id)
            .map(Box::as_ref)
    }

    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = &dyn UsageProvider> {
        self.sources.iter().map(Box::as_ref)
    }
    /// Sources stay in registration order; built-ins keep their established first slots.
    pub fn handles(&'static self) -> impl Iterator<Item = ProviderHandle> {
        self.sources
            .iter()
            .enumerate()
            .map(|(index, source)| ProviderHandle {
                index,
                source: source.as_ref(),
            })
    }
}

/// Resolving roots can walk thousands of directories, so it is redone only when the
/// configuration changes or every few minutes (new Claude Desktop sessions appear).
struct Scanner {
    custom: String,
    roots: Option<(f64, super::logs::Roots)>,
    cache: super::logs::Cache,
}

struct LocalProvider {
    provider: Provider,
    scanner: Mutex<Scanner>,
    report_roots: Mutex<String>,
}

impl UsageProvider for LocalProvider {
    fn id(&self) -> &str {
        match self.provider {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }

    fn name(&self) -> &str {
        self.provider.name()
    }

    fn remaining_usage(&self) -> Result<Vec<Allowance>, Failure> {
        super::query(self.provider)
    }

    fn activity(&self, now: f64) -> Daily {
        self.observations(now).activity
    }
    fn supports_custom_roots(&self) -> bool {
        true
    }
    fn configure_scan_roots(&self, raw: &str) -> bool {
        let mut roots = self.report_roots.lock().unwrap_or_else(|e| e.into_inner());
        let raw = raw.trim();
        if *roots == raw {
            return false;
        }
        *roots = raw.to_owned();
        true
    }
    /// One pass over the logs feeds both the usage report and the heatmap.
    fn observations(&self, now: f64) -> Observation {
        let raw = self
            .report_roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut scanner = self.scanner.lock().unwrap_or_else(|e| e.into_inner());
        let scanner = &mut *scanner;
        if scanner.custom != raw {
            *scanner = Scanner {
                custom: raw,
                roots: None,
                cache: Default::default(),
            };
        }
        let roots = match &scanner.roots {
            Some((at, roots)) if (0. ..ROOTS_TTL).contains(&(now - at)) => roots,
            _ => {
                let roots = super::logs::Roots::for_provider_with_custom(
                    self.provider,
                    Some(&scanner.custom),
                );
                &scanner.roots.insert((now, roots)).1
            }
        };
        let report = super::logs::scan(self.provider, roots, &mut scanner.cache, now);
        Observation {
            activity: report.heatmap(),
            report,
        }
    }
}

const ROOTS_TTL: f64 = 300.;

pub fn builtins() -> &'static Registry {
    static SOURCES: OnceLock<Registry> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let mut sources = Registry::default();
        for provider in Provider::ALL {
            sources
                .register(LocalProvider {
                    provider,
                    scanner: Mutex::new(Scanner {
                        custom: String::new(),
                        roots: None,
                        cache: Default::default(),
                    }),
                    report_roots: Mutex::default(),
                })
                .expect("unique built-in provider ID");
        }
        sources
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FutureTool(&'static str);

    impl UsageProvider for FutureTool {
        fn id(&self) -> &str {
            self.0
        }
        fn name(&self) -> &str {
            "Future coding tool"
        }
        fn remaining_usage(&self) -> Result<Vec<Allowance>, Failure> {
            Err(Failure::Unsupported)
        }
        fn activity(&self, _: f64) -> Daily {
            Daily {
                first_day: 42,
                tokens: vec![7, 11],
            }
        }
    }

    #[test]
    fn a_new_tool_registers_without_extending_the_builtin_enum() {
        let mut registry = Registry::default();
        registry.register(FutureTool("future-tool")).unwrap();
        let source = registry.get("future-tool").unwrap();
        assert_eq!(source.name(), "Future coding tool");
        assert_eq!(
            source.activity(0.),
            Daily {
                first_day: 42,
                tokens: vec![7, 11]
            }
        );
        assert_eq!(source.remaining_usage(), Err(Failure::Unsupported));
        assert_eq!(registry.iter().count(), 1);
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn registry_rejects_duplicate_and_unstable_ids() {
        let mut registry = Registry::default();
        registry.register(FutureTool("tool")).unwrap();
        assert!(registry.register(FutureTool("tool")).is_err());
        for id in ["", "Tool", "tool name", "../tool", "tööl"] {
            assert!(registry.register(FutureTool(id)).is_err());
        }
        assert_eq!(registry.iter().count(), 1);
    }

    #[test]
    fn builtins_have_stable_ids_and_keep_sharks_account_readers() {
        assert_eq!(
            builtins().iter().map(|p| p.id()).collect::<Vec<_>>(),
            ["claude", "codex"]
        );
        assert_eq!(Provider::Claude.source().name(), Provider::Claude.name());
        assert_eq!(Provider::Codex.source().name(), Provider::Codex.name());
        // Do not call the live readers here: protocol/fixture tests in usage::tests cover
        // the unchanged query implementation without accessing the user's accounts.
    }
}
