//! Choosing which installed version of a component to launch.
//!
//! The rule (PLAN.md §4 Phase 2, Manager §00.8): **a per-project pin overrides
//! the global active version.** A project that pins its framework builds with
//! that framework on every machine; a project that pins nothing follows
//! whatever `cln use` last selected.
//!
//! Resolution failures are the most common thing a new user hits, so every
//! error here carries the `cln` command that fixes it.

use std::path::PathBuf;

use cln_layout::Layout;
use cln_project::{Pins, PinsError};
use cln_shared::ToolchainKind;
use semver::Version;

/// Where a resolved version came from — reported so `--verbose` can explain
/// *why* a particular binary was chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionSource {
    /// A per-project pin file.
    Pin,
    /// The global `~/.cln/active/<kind>` symlink.
    Active,
}

impl VersionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionSource::Pin => "project pin",
            VersionSource::Active => "active version",
        }
    }
}

/// A component binary that exists on disk and is ready to spawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    pub kind: ToolchainKind,
    pub version: Version,
    pub binary: PathBuf,
    pub source: VersionSource,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no {kind} version selected")]
    NoVersion { kind: ToolchainKind },

    #[error("{kind} {version} is pinned by this project but is not installed")]
    PinnedButMissing {
        kind: ToolchainKind,
        version: Version,
        expected: PathBuf,
    },

    #[error("{kind} {version} is selected but its binary is missing from {}", .expected.display())]
    ActiveButMissing {
        kind: ToolchainKind,
        version: Version,
        expected: PathBuf,
    },

    #[error(transparent)]
    Pins(#[from] PinsError),
}

impl ResolveError {
    /// The command that fixes this, shown as the `help:` line. Mirrors the
    /// framework's `ResolveError::remedy` so both components coach identically.
    pub fn remedy(&self) -> Option<String> {
        match self {
            ResolveError::NoVersion { kind } => Some(format!(
                "run `cln install {kind} latest` to install one, \
                 or `cln use {kind} <version>` to select an installed version"
            )),
            ResolveError::PinnedButMissing { kind, version, .. } => {
                Some(format!("run `cln install {kind} {version}`"))
            }
            ResolveError::ActiveButMissing { kind, version, .. } => Some(format!(
                "the install looks damaged; run `cln install {kind} {version}` to repair it"
            )),
            ResolveError::Pins(_) => None,
        }
    }
}

/// Resolve the binary for `kind`, preferring `project_root`'s pin over the
/// globally active version.
///
/// `project_root` is optional because not every dispatched verb runs inside a
/// project; with `None`, only the global active version is consulted.
pub fn resolve_component(
    kind: ToolchainKind,
    project_root: Option<&std::path::Path>,
    layout: &Layout,
) -> Result<Resolved, ResolveError> {
    let pinned = match project_root {
        Some(root) => Pins::new(root).get(kind)?,
        None => None,
    };

    let (version, source) = match pinned {
        Some(v) => (v, VersionSource::Pin),
        None => match layout.active_version(kind) {
            Some(v) => (v, VersionSource::Active),
            None => return Err(ResolveError::NoVersion { kind }),
        },
    };

    let binary = layout.version_binary(kind, &version);
    if !binary.is_file() {
        // Same missing binary, different cause and different fix: a pin points
        // at something never installed, while a dangling active link means the
        // install was damaged after the fact.
        return Err(match source {
            VersionSource::Pin => ResolveError::PinnedButMissing {
                kind,
                version,
                expected: binary,
            },
            VersionSource::Active => ResolveError::ActiveButMissing {
                kind,
                version,
                expected: binary,
            },
        });
    }

    Ok(Resolved {
        kind,
        version,
        binary,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    /// Install a stand-in binary for `kind` at `version`.
    fn install(layout: &Layout, kind: ToolchainKind, version: &Version) {
        std::fs::create_dir_all(layout.version_dir(kind, version)).unwrap();
        std::fs::write(layout.version_binary(kind, version), b"stub").unwrap();
    }

    #[test]
    fn uses_the_active_version_when_the_project_pins_nothing() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        install(&layout, ToolchainKind::Framework, &v("0.1.1"));
        layout
            .set_active(ToolchainKind::Framework, &v("0.1.1"))
            .unwrap();

        let proj = tempdir().unwrap();
        let r = resolve_component(ToolchainKind::Framework, Some(proj.path()), &layout).unwrap();
        assert_eq!(r.version, v("0.1.1"));
        assert_eq!(r.source, VersionSource::Active);
    }

    #[test]
    fn a_project_pin_overrides_the_active_version() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        install(&layout, ToolchainKind::Framework, &v("0.1.1"));
        install(&layout, ToolchainKind::Framework, &v("0.2.0"));
        layout
            .set_active(ToolchainKind::Framework, &v("0.1.1"))
            .unwrap();

        let proj = tempdir().unwrap();
        Pins::new(proj.path())
            .set(ToolchainKind::Framework, &v("0.2.0"))
            .unwrap();

        let r = resolve_component(ToolchainKind::Framework, Some(proj.path()), &layout).unwrap();
        assert_eq!(r.version, v("0.2.0"), "the pin must win");
        assert_eq!(r.source, VersionSource::Pin);
        assert_eq!(
            r.binary,
            layout.version_binary(ToolchainKind::Framework, &v("0.2.0"))
        );
    }

    #[test]
    fn without_a_project_only_the_active_version_is_consulted() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        install(&layout, ToolchainKind::Framework, &v("0.1.1"));
        layout
            .set_active(ToolchainKind::Framework, &v("0.1.1"))
            .unwrap();

        let r = resolve_component(ToolchainKind::Framework, None, &layout).unwrap();
        assert_eq!(r.source, VersionSource::Active);
    }

    #[test]
    fn nothing_installed_and_nothing_pinned_is_a_no_version_error() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();

        let err = resolve_component(ToolchainKind::Framework, None, &layout).unwrap_err();
        assert!(matches!(err, ResolveError::NoVersion { .. }));
        assert!(err
            .remedy()
            .unwrap()
            .contains("cln install framework latest"));
    }

    #[test]
    fn a_pin_to_an_uninstalled_version_names_the_install_command() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();

        let proj = tempdir().unwrap();
        Pins::new(proj.path())
            .set(ToolchainKind::Framework, &v("9.9.9"))
            .unwrap();

        let err =
            resolve_component(ToolchainKind::Framework, Some(proj.path()), &layout).unwrap_err();
        assert!(matches!(err, ResolveError::PinnedButMissing { .. }));
        assert_eq!(
            err.remedy().unwrap(),
            "run `cln install framework 9.9.9`",
            "the fix must name the pinned version, not latest"
        );
    }

    #[test]
    fn a_dangling_active_link_reports_a_damaged_install() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        install(&layout, ToolchainKind::Framework, &v("0.1.1"));
        layout
            .set_active(ToolchainKind::Framework, &v("0.1.1"))
            .unwrap();
        // Remove the binary but leave the symlink pointing at the directory.
        std::fs::remove_file(layout.version_binary(ToolchainKind::Framework, &v("0.1.1"))).unwrap();

        let err = resolve_component(ToolchainKind::Framework, None, &layout).unwrap_err();
        assert!(matches!(err, ResolveError::ActiveButMissing { .. }));
        assert!(err.remedy().unwrap().contains("repair"));
    }

    #[test]
    fn a_malformed_pin_surfaces_rather_than_falling_back() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        install(&layout, ToolchainKind::Framework, &v("0.1.1"));
        layout
            .set_active(ToolchainKind::Framework, &v("0.1.1"))
            .unwrap();

        let proj = tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join(".cln")).unwrap();
        std::fs::write(proj.path().join(".cln/frame-version"), "garbage").unwrap();

        // Falling back to the active version here would build against a
        // different toolchain than the project asked for.
        let err =
            resolve_component(ToolchainKind::Framework, Some(proj.path()), &layout).unwrap_err();
        assert!(matches!(err, ResolveError::Pins(_)));
    }

    #[test]
    fn pins_for_other_kinds_do_not_affect_this_one() {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        install(&layout, ToolchainKind::Framework, &v("0.1.1"));
        layout
            .set_active(ToolchainKind::Framework, &v("0.1.1"))
            .unwrap();

        let proj = tempdir().unwrap();
        // A compiler pin is the framework's business, not ours.
        Pins::new(proj.path())
            .set(ToolchainKind::Compiler, &v("7.7.7"))
            .unwrap();

        let r = resolve_component(ToolchainKind::Framework, Some(proj.path()), &layout).unwrap();
        assert_eq!(r.version, v("0.1.1"));
    }
}
