//! Mapping from toolchain kind to the GitHub repo that publishes its releases.
//!
//! Defaults are the canonical Clean Language repositories. Any of the three
//! can be overridden with an env var for local development, staging, or CI
//! against a fork:
//!
//! ```text
//! CLN_COMPILER_REPO=my-fork/clean-language-compiler
//! CLN_FRAMEWORK_REPO=my-fork/clean-framework
//! CLN_RUNTIME_REPO=my-fork/clean-runtime
//! ```

use cln_shared::ToolchainKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn parse(s: &str) -> Option<Self> {
        let (owner, name) = s.split_once('/')?;
        if owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(Self { owner: owner.into(), name: name.into() })
    }
}

/// The default GitHub `owner/name` for a kind, before env-var overrides.
pub fn default_repo(kind: ToolchainKind) -> RepoRef {
    match kind {
        ToolchainKind::Compiler => RepoRef {
            owner: "clean-language".into(),
            name: "clean-language-compiler".into(),
        },
        ToolchainKind::Framework => RepoRef {
            owner: "clean-language".into(),
            name: "clean-framework".into(),
        },
        ToolchainKind::Runtime => RepoRef {
            owner: "clean-language".into(),
            name: "clean-runtime".into(),
        },
    }
}

/// The env-var name that overrides the repo for a given kind.
pub fn env_var(kind: ToolchainKind) -> &'static str {
    match kind {
        ToolchainKind::Compiler => "CLN_COMPILER_REPO",
        ToolchainKind::Framework => "CLN_FRAMEWORK_REPO",
        ToolchainKind::Runtime => "CLN_RUNTIME_REPO",
    }
}

/// Resolve the effective repo for a kind, honoring the env-var override.
pub fn resolve_repo(kind: ToolchainKind) -> RepoRef {
    if let Ok(val) = std::env::var(env_var(kind)) {
        if let Some(r) = RepoRef::parse(val.trim()) {
            return r;
        }
    }
    default_repo(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_owner_slash_name() {
        assert_eq!(
            RepoRef::parse("foo/bar"),
            Some(RepoRef { owner: "foo".into(), name: "bar".into() })
        );
    }

    #[test]
    fn parse_rejects_malformed() {
        assert_eq!(RepoRef::parse("nosep"), None);
        assert_eq!(RepoRef::parse("/bar"), None);
        assert_eq!(RepoRef::parse("foo/"), None);
    }

    #[test]
    fn defaults_are_populated() {
        for k in ToolchainKind::ALL {
            let r = default_repo(k);
            assert!(!r.owner.is_empty());
            assert!(!r.name.is_empty());
        }
    }
}
