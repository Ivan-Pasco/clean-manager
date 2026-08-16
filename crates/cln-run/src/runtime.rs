//! Choosing which installed runtime to invoke (Manager §00.13, "Version
//! resolution and compatibility").
//!
//! The chain, in order:
//!
//! 1. **Artifact manifest pin.** If the bundle names a runtime version, that
//!    version must be installed — no fallback.
//! 2. **Project pin.** `.cln/runtime-version`, when running inside a project.
//! 3. **Global active runtime.** `~/.cln/active/runtime`.
//!
//! This is deliberately *not* [`cln_dispatch::resolve_component`]. That
//! function answers "which framework does this project want", a two-step
//! pin-then-active question with no artifact in it. Here the artifact outranks
//! the project, because a bundle is a built thing that was compiled against one
//! runtime's host contract and carries no guarantee against another's — the
//! machine's preference cannot overrule what the artifact was built for. The
//! two crates share the *shape* of the answer, not the rule.
//!
//! # When a pin is missing
//!
//! §00.13 says `cln run` "prompts to install it and exits". Manager fails with
//! the exact `cln install runtime <version>` command instead of prompting.
//! `cln run` is used non-interactively — by CI, by scripts, by a double-click
//! with no terminal attached — and every child manager spawns already gets a
//! closed stdin. Blocking on a read that can never be answered would hang those
//! callers, and installing without being asked is the surprise PLAN.md's open
//! question 9 rules out for builds. Failing with the command keeps the user one
//! copy-paste from running, and keeps `--offline` meaningful.

use std::path::{Path, PathBuf};

use cln_layout::Layout;
use cln_project::{Pins, PinsError};
use cln_shared::ToolchainKind;
use semver::Version;

/// Where the chosen runtime version came from. Reported by `--verbose` so a
/// surprising choice can be traced to the file that caused it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSource {
    /// The artifact's `manifest.toml` named an exact version.
    ArtifactPin,
    /// The project's `.cln/runtime-version`.
    ProjectPin,
    /// The global `~/.cln/active/runtime` symlink.
    Active,
}

impl RuntimeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeSource::ArtifactPin => "artifact pin",
            RuntimeSource::ProjectPin => "project pin",
            RuntimeSource::Active => "active version",
        }
    }
}

/// A runtime binary that exists on disk and is ready to spawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRuntime {
    pub version: Version,
    pub binary: PathBuf,
    pub source: RuntimeSource,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("this artifact was built against runtime {version}, which is not installed")]
    ArtifactPinMissing { version: Version, expected: PathBuf },

    #[error("runtime {version} is pinned by this project but is not installed")]
    ProjectPinMissing { version: Version, expected: PathBuf },

    #[error("no runtime is installed")]
    NoRuntime,

    #[error("runtime {version} is selected but its binary is missing from {}", .expected.display())]
    ActiveButMissing { version: Version, expected: PathBuf },

    #[error(transparent)]
    Pins(#[from] PinsError),
}

impl RuntimeError {
    /// The command that fixes this. Every variant that a user can act on names
    /// one, matching how `cln-dispatch::ResolveError` coaches.
    pub fn remedy(&self) -> Option<String> {
        match self {
            RuntimeError::ArtifactPinMissing { version, .. }
            | RuntimeError::ProjectPinMissing { version, .. } => {
                Some(format!("run `cln install runtime {version}`"))
            }
            RuntimeError::NoRuntime => {
                Some("run `cln install runtime latest` to install one".into())
            }
            RuntimeError::ActiveButMissing { version, .. } => Some(format!(
                "the install looks damaged; run `cln install runtime {version}` to repair it"
            )),
            RuntimeError::Pins(_) => None,
        }
    }
}

/// Resolve the runtime to invoke.
///
/// `artifact_pin` is the manifest's version, when it names one — see
/// [`crate::manifest::Manifest::runtime_pin`] for why a manifest can decline to
/// pin. `project_root` is the project whose `.cln/runtime-version` applies, if
/// the run is happening inside one.
pub fn resolve_runtime(
    artifact_pin: Option<&Version>,
    project_root: Option<&Path>,
    layout: &Layout,
) -> Result<ResolvedRuntime, RuntimeError> {
    // 1. The artifact's pin outranks everything and never falls through: a
    //    component built against one host contract has no guarantee against
    //    another, so substituting a different runtime would run it against
    //    imports it was not checked for.
    if let Some(version) = artifact_pin {
        let binary = layout.version_binary(ToolchainKind::Runtime, version);
        return if binary.is_file() {
            Ok(ResolvedRuntime {
                version: version.clone(),
                binary,
                source: RuntimeSource::ArtifactPin,
            })
        } else {
            Err(RuntimeError::ArtifactPinMissing {
                version: version.clone(),
                expected: binary,
            })
        };
    }

    // 2. The project's pin.
    if let Some(root) = project_root {
        if let Some(version) = Pins::new(root).get(ToolchainKind::Runtime)? {
            let binary = layout.version_binary(ToolchainKind::Runtime, &version);
            return if binary.is_file() {
                Ok(ResolvedRuntime {
                    version,
                    binary,
                    source: RuntimeSource::ProjectPin,
                })
            } else {
                Err(RuntimeError::ProjectPinMissing {
                    version,
                    expected: binary,
                })
            };
        }
    }

    // 3. Whatever `cln use runtime` last selected.
    let version = layout
        .active_version(ToolchainKind::Runtime)
        .ok_or(RuntimeError::NoRuntime)?;
    let binary = layout.version_binary(ToolchainKind::Runtime, &version);
    if !binary.is_file() {
        return Err(RuntimeError::ActiveButMissing {
            version,
            expected: binary,
        });
    }

    Ok(ResolvedRuntime {
        version,
        binary,
        source: RuntimeSource::Active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    fn install(layout: &Layout, version: &Version) {
        std::fs::create_dir_all(layout.version_dir(ToolchainKind::Runtime, version)).unwrap();
        std::fs::write(
            layout.version_binary(ToolchainKind::Runtime, version),
            b"stub",
        )
        .unwrap();
    }

    fn layout_with(versions: &[&str], active: Option<&str>) -> (tempfile::TempDir, Layout) {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        for s in versions {
            install(&layout, &v(s));
        }
        if let Some(a) = active {
            layout.set_active(ToolchainKind::Runtime, &v(a)).unwrap();
        }
        (home, layout)
    }

    #[test]
    fn the_artifact_pin_wins_over_everything() {
        let (_h, layout) = layout_with(&["1.0.0", "2.0.0"], Some("1.0.0"));
        let proj = tempdir().unwrap();
        Pins::new(proj.path())
            .set(ToolchainKind::Runtime, &v("1.0.0"))
            .unwrap();

        let r = resolve_runtime(Some(&v("2.0.0")), Some(proj.path()), &layout).unwrap();
        assert_eq!(r.version, v("2.0.0"));
        assert_eq!(r.source, RuntimeSource::ArtifactPin);
    }

    /// The artifact pin must not silently degrade to the active runtime — the
    /// component was checked against one host contract, not another.
    #[test]
    fn a_missing_artifact_pin_fails_rather_than_falling_back() {
        let (_h, layout) = layout_with(&["1.0.0"], Some("1.0.0"));

        let err = resolve_runtime(Some(&v("9.9.9")), None, &layout).unwrap_err();
        assert!(matches!(err, RuntimeError::ArtifactPinMissing { .. }));
        assert_eq!(err.remedy().unwrap(), "run `cln install runtime 9.9.9`");
    }

    #[test]
    fn the_project_pin_is_used_when_the_artifact_declines_to_pin() {
        let (_h, layout) = layout_with(&["1.0.0", "2.0.0"], Some("1.0.0"));
        let proj = tempdir().unwrap();
        Pins::new(proj.path())
            .set(ToolchainKind::Runtime, &v("2.0.0"))
            .unwrap();

        let r = resolve_runtime(None, Some(proj.path()), &layout).unwrap();
        assert_eq!(r.version, v("2.0.0"));
        assert_eq!(r.source, RuntimeSource::ProjectPin);
    }

    #[test]
    fn the_active_runtime_is_the_last_resort() {
        let (_h, layout) = layout_with(&["1.0.0"], Some("1.0.0"));
        let proj = tempdir().unwrap();

        let r = resolve_runtime(None, Some(proj.path()), &layout).unwrap();
        assert_eq!(r.version, v("1.0.0"));
        assert_eq!(r.source, RuntimeSource::Active);
    }

    /// A `.clapp` run from anywhere has no project; only the global active
    /// runtime applies.
    #[test]
    fn without_a_project_only_the_active_runtime_is_consulted() {
        let (_h, layout) = layout_with(&["1.0.0"], Some("1.0.0"));
        let r = resolve_runtime(None, None, &layout).unwrap();
        assert_eq!(r.source, RuntimeSource::Active);
    }

    #[test]
    fn nothing_installed_names_the_install_command() {
        let (_h, layout) = layout_with(&[], None);
        let err = resolve_runtime(None, None, &layout).unwrap_err();
        assert!(matches!(err, RuntimeError::NoRuntime));
        assert!(err.remedy().unwrap().contains("cln install runtime latest"));
    }

    #[test]
    fn a_project_pin_to_an_uninstalled_version_names_that_version() {
        let (_h, layout) = layout_with(&["1.0.0"], Some("1.0.0"));
        let proj = tempdir().unwrap();
        Pins::new(proj.path())
            .set(ToolchainKind::Runtime, &v("3.3.3"))
            .unwrap();

        let err = resolve_runtime(None, Some(proj.path()), &layout).unwrap_err();
        assert!(matches!(err, RuntimeError::ProjectPinMissing { .. }));
        assert_eq!(err.remedy().unwrap(), "run `cln install runtime 3.3.3`");
    }

    #[test]
    fn a_dangling_active_link_reports_a_damaged_install() {
        let (_h, layout) = layout_with(&["1.0.0"], Some("1.0.0"));
        std::fs::remove_file(layout.version_binary(ToolchainKind::Runtime, &v("1.0.0"))).unwrap();

        let err = resolve_runtime(None, None, &layout).unwrap_err();
        assert!(matches!(err, RuntimeError::ActiveButMissing { .. }));
        assert!(err.remedy().unwrap().contains("repair"));
    }

    /// A malformed pin must surface rather than silently falling through to a
    /// different runtime than the project asked for.
    #[test]
    fn a_malformed_project_pin_surfaces() {
        let (_h, layout) = layout_with(&["1.0.0"], Some("1.0.0"));
        let proj = tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join(".cln")).unwrap();
        std::fs::write(proj.path().join(".cln/runtime-version"), "not-a-version").unwrap();

        let err = resolve_runtime(None, Some(proj.path()), &layout).unwrap_err();
        assert!(matches!(err, RuntimeError::Pins(_)));
    }

    /// A compiler or framework pin says nothing about which runtime to use.
    #[test]
    fn pins_for_other_kinds_are_ignored() {
        let (_h, layout) = layout_with(&["1.0.0"], Some("1.0.0"));
        let proj = tempdir().unwrap();
        Pins::new(proj.path())
            .set(ToolchainKind::Framework, &v("7.7.7"))
            .unwrap();

        let r = resolve_runtime(None, Some(proj.path()), &layout).unwrap();
        assert_eq!(r.version, v("1.0.0"));
        assert_eq!(r.source, RuntimeSource::Active);
    }
}
