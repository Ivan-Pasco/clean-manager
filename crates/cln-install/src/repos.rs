//! Mapping from toolchain kind to the GitHub repo that publishes its releases.
//!
//! Defaults are the canonical Clean Language repositories, all owned by
//! [`DEFAULT_OWNER`]. Any of the three can be overridden with an env var for
//! local development, staging, or CI against a fork:
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

/// The GitHub account that owns the toolchain repositories.
///
/// This was `clean-language` — an organization that does not exist. Every
/// `cln install` against a default resolved to a 404. The repositories live
/// under the personal account that also owns clean-manager itself; if they
/// move to an organization later, this constant is the single edit.
pub const DEFAULT_OWNER: &str = "Ivan-Pasco";

/// The default GitHub `owner/name` for a kind, before env-var overrides.
pub fn default_repo(kind: ToolchainKind) -> RepoRef {
    let name = match kind {
        ToolchainKind::Compiler => "clean-language-compiler",
        ToolchainKind::Framework => "clean-framework",
        ToolchainKind::Runtime => "clean-runtime",
    };
    RepoRef { owner: DEFAULT_OWNER.into(), name: name.into() }
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
    resolve_repo_from(kind, std::env::var(env_var(kind)).ok().as_deref())
}

/// The override logic itself, with the environment passed in.
///
/// Split from [`resolve_repo`] so it is testable without mutating
/// process-global state: these vars are read by `GithubReleases::new`, so a
/// test that set them for real would race with any concurrent test that
/// constructs one.
///
/// A malformed override falls back to the default rather than failing the
/// install — an unparseable value cannot name a real repository, and the
/// default at least has a chance of being right.
pub fn resolve_repo_from(kind: ToolchainKind, override_value: Option<&str>) -> RepoRef {
    if let Some(value) = override_value {
        if let Some(parsed) = RepoRef::parse(value.trim()) {
            return parsed;
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

    #[test]
    fn defaults_name_the_repositories_that_actually_publish_releases() {
        // The previous version of this test only asserted the fields were
        // non-empty, which is why `clean-language` — an owner that does not
        // exist on GitHub — shipped and made every default `cln install`
        // resolve to a 404. Pin the exact strings so a wrong owner is a
        // failing test rather than a runtime 404 on a user's machine.
        assert_eq!(
            default_repo(ToolchainKind::Compiler),
            RepoRef { owner: "Ivan-Pasco".into(), name: "clean-language-compiler".into() }
        );
        assert_eq!(
            default_repo(ToolchainKind::Framework),
            RepoRef { owner: "Ivan-Pasco".into(), name: "clean-framework".into() }
        );
        assert_eq!(
            default_repo(ToolchainKind::Runtime),
            RepoRef { owner: "Ivan-Pasco".into(), name: "clean-runtime".into() }
        );
    }

    #[test]
    fn every_kind_shares_the_default_owner() {
        // If the repos move to an organization, DEFAULT_OWNER is the one edit.
        // This fails if someone hard-codes a divergent owner for a single kind.
        for k in ToolchainKind::ALL {
            assert_eq!(default_repo(k).owner, DEFAULT_OWNER, "{k} diverges from DEFAULT_OWNER");
        }
    }

    #[test]
    fn override_is_applied_when_well_formed_and_ignored_when_not() {
        // Guards the escape hatch the module doc promises: without it, a fork
        // or a staging repo cannot be installed from at all.
        //
        // Exercised through `resolve_repo_from` rather than by setting the
        // real env var — `GithubReleases::new` reads these vars, so mutating
        // process-global state here would race with any concurrent test that
        // constructs one.
        let default = default_repo(ToolchainKind::Framework);

        assert_eq!(
            resolve_repo_from(ToolchainKind::Framework, Some("my-fork/clean-framework")),
            RepoRef { owner: "my-fork".into(), name: "clean-framework".into() }
        );

        // Surrounding whitespace is tolerated — a shell export easily adds it.
        assert_eq!(
            resolve_repo_from(ToolchainKind::Framework, Some("  my-fork/clean-framework \n")),
            RepoRef { owner: "my-fork".into(), name: "clean-framework".into() }
        );

        // A malformed override falls back to the default rather than
        // producing a nonsense repo reference.
        for malformed in ["not-a-repo", "/clean-framework", "my-fork/", ""] {
            assert_eq!(
                resolve_repo_from(ToolchainKind::Framework, Some(malformed)),
                default,
                "malformed override {malformed:?} must fall back to the default"
            );
        }

        assert_eq!(resolve_repo_from(ToolchainKind::Framework, None), default);
    }
}
