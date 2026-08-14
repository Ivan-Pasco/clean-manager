//! Per-project toolchain pins.
//!
//! A project can pin each toolchain kind to an exact version, overriding the
//! globally active one (PLAN.md §4 Phase 2). One file per kind, each holding a
//! bare semver string:
//!
//! ```text
//! .cln/version           compiler
//! .cln/frame-version     framework
//! .cln/runtime-version   runtime
//! ```
//!
//! **The compiler pin is a shared contract.** The framework reads
//! `.cln/version` itself to locate the compiler it shells out to
//! (`framework-compiler-driver::resolve`), so the format here is not ours to
//! change unilaterally: a bare semver string, surrounding whitespace tolerated.
//! We write a trailing newline because editors and `cat` expect one, and the
//! reader on both sides trims.
//!
//! Toolchain versions live in these files rather than in `clean.toml` because
//! Platform 07 §7.2 keeps toolchain state out of project configuration — a
//! checked-in `clean.toml` describes the project, while pins describe the
//! machine's view of it and may legitimately differ per checkout.

use std::io;
use std::path::{Path, PathBuf};

use cln_shared::ToolchainKind;
use semver::Version;

#[derive(Debug, thiserror::Error)]
pub enum PinsError {
    #[error("could not read {}: {source}", .path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not write {}: {source}", .path.display())]
    Unwritable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{} contains '{raw}', which is not a version: {source}", .path.display())]
    Malformed {
        path: PathBuf,
        raw: String,
        #[source]
        source: semver::Error,
    },
}

/// The pin file name for a kind, relative to the project root.
///
/// The compiler's is `.cln/version` rather than `.cln/compiler-version` for
/// historical reasons — it is the name the framework already reads.
pub fn pin_file(kind: ToolchainKind) -> &'static str {
    match kind {
        ToolchainKind::Compiler => ".cln/version",
        ToolchainKind::Framework => ".cln/frame-version",
        ToolchainKind::Runtime => ".cln/runtime-version",
    }
}

/// Read/write access to one project's pins.
#[derive(Clone, Debug)]
pub struct Pins {
    root: PathBuf,
}

impl Pins {
    /// Pins for the project rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The absolute path of one pin file.
    pub fn path(&self, kind: ToolchainKind) -> PathBuf {
        self.root.join(pin_file(kind))
    }

    /// The pinned version for a kind.
    ///
    /// `Ok(None)` means "not pinned" — the common case, and not an error; the
    /// caller falls back to the globally active version. A file that exists but
    /// holds garbage *is* an error, because silently ignoring it would build
    /// against a different toolchain than the one the project asked for.
    pub fn get(&self, kind: ToolchainKind) -> Result<Option<Version>, PinsError> {
        let path = self.path(kind);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(PinsError::Unreadable { path, source }),
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        trimmed
            .parse::<Version>()
            .map(Some)
            .map_err(|source| PinsError::Malformed {
                path,
                raw: trimmed.to_string(),
                source,
            })
    }

    /// Pin a kind to an exact version, creating `.cln/` if needed.
    pub fn set(&self, kind: ToolchainKind, version: &Version) -> Result<(), PinsError> {
        let path = self.path(kind);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PinsError::Unwritable {
                path: path.clone(),
                source,
            })?;
        }
        std::fs::write(&path, format!("{version}\n")).map_err(|source| PinsError::Unwritable {
            path: path.clone(),
            source,
        })
    }

    /// Remove a pin. Removing one that isn't there is a no-op, not an error.
    pub fn clear(&self, kind: ToolchainKind) -> Result<(), PinsError> {
        let path = self.path(kind);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PinsError::Unwritable { path, source }),
        }
    }

    /// Every pin this project declares, in `ToolchainKind::ALL` order.
    pub fn all(&self) -> Result<Vec<(ToolchainKind, Option<Version>)>, PinsError> {
        ToolchainKind::ALL
            .into_iter()
            .map(|k| self.get(k).map(|v| (k, v)))
            .collect()
    }

    /// The project root these pins belong to.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn unpinned_reads_as_none() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path());
        for k in ToolchainKind::ALL {
            assert_eq!(pins.get(k).unwrap(), None);
        }
    }

    #[test]
    fn set_then_get_roundtrips() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path());
        pins.set(ToolchainKind::Framework, &v("0.1.1")).unwrap();
        assert_eq!(
            pins.get(ToolchainKind::Framework).unwrap(),
            Some(v("0.1.1"))
        );
    }

    #[test]
    fn set_creates_the_cln_directory() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path().join("proj"));
        pins.set(ToolchainKind::Compiler, &v("1.2.3")).unwrap();
        assert!(tmp.path().join("proj").join(".cln").is_dir());
    }

    #[test]
    fn kinds_use_distinct_files() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path());
        pins.set(ToolchainKind::Compiler, &v("1.0.0")).unwrap();
        pins.set(ToolchainKind::Framework, &v("2.0.0")).unwrap();
        pins.set(ToolchainKind::Runtime, &v("3.0.0")).unwrap();

        assert_eq!(pins.get(ToolchainKind::Compiler).unwrap(), Some(v("1.0.0")));
        assert_eq!(
            pins.get(ToolchainKind::Framework).unwrap(),
            Some(v("2.0.0"))
        );
        assert_eq!(pins.get(ToolchainKind::Runtime).unwrap(), Some(v("3.0.0")));
    }

    /// The framework reads `.cln/version` with its own parser; if we ever stop
    /// writing a bare trimmed semver there, its builds break, so pin the shape.
    #[test]
    fn compiler_pin_is_a_bare_semver_line_at_the_agreed_path() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path());
        pins.set(ToolchainKind::Compiler, &v("1.4.0")).unwrap();

        let raw = std::fs::read_to_string(tmp.path().join(".cln/version")).unwrap();
        assert_eq!(raw, "1.4.0\n");
        assert_eq!(raw.trim().parse::<Version>().unwrap(), v("1.4.0"));
    }

    #[test]
    fn pin_paths_match_the_documented_names() {
        assert_eq!(pin_file(ToolchainKind::Compiler), ".cln/version");
        assert_eq!(pin_file(ToolchainKind::Framework), ".cln/frame-version");
        assert_eq!(pin_file(ToolchainKind::Runtime), ".cln/runtime-version");
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cln")).unwrap();
        std::fs::write(tmp.path().join(".cln/frame-version"), "  0.2.0 \n\n").unwrap();

        let pins = Pins::new(tmp.path());
        assert_eq!(
            pins.get(ToolchainKind::Framework).unwrap(),
            Some(v("0.2.0"))
        );
    }

    #[test]
    fn an_empty_pin_file_counts_as_unpinned() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cln")).unwrap();
        std::fs::write(tmp.path().join(".cln/frame-version"), "\n  \n").unwrap();

        let pins = Pins::new(tmp.path());
        assert_eq!(pins.get(ToolchainKind::Framework).unwrap(), None);
    }

    #[test]
    fn a_malformed_pin_is_an_error_not_a_silent_fallback() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cln")).unwrap();
        std::fs::write(tmp.path().join(".cln/frame-version"), "not-a-version").unwrap();

        let pins = Pins::new(tmp.path());
        assert!(matches!(
            pins.get(ToolchainKind::Framework),
            Err(PinsError::Malformed { .. })
        ));
    }

    #[test]
    fn prerelease_pins_survive_the_roundtrip() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path());
        pins.set(ToolchainKind::Framework, &v("0.2.0-rc.1"))
            .unwrap();
        assert_eq!(
            pins.get(ToolchainKind::Framework).unwrap(),
            Some(v("0.2.0-rc.1"))
        );
    }

    #[test]
    fn clear_removes_the_pin_and_is_idempotent() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path());
        pins.set(ToolchainKind::Runtime, &v("1.0.0")).unwrap();
        pins.clear(ToolchainKind::Runtime).unwrap();
        assert_eq!(pins.get(ToolchainKind::Runtime).unwrap(), None);
        pins.clear(ToolchainKind::Runtime).unwrap();
    }

    #[test]
    fn all_reports_every_kind() {
        let tmp = tempdir().unwrap();
        let pins = Pins::new(tmp.path());
        pins.set(ToolchainKind::Framework, &v("0.1.1")).unwrap();

        let all = pins.all().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter()
                .find(|(k, _)| *k == ToolchainKind::Framework)
                .unwrap()
                .1,
            Some(v("0.1.1"))
        );
    }
}
