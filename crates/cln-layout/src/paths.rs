use std::io;
use std::path::{Path, PathBuf};

use cln_shared::ToolchainKind;

/// A handle rooted at `~/.cln/` (or a test tempdir). Every path this crate
/// exposes is a descendant of `root`.
#[derive(Clone, Debug)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Build a layout rooted at the given path. No I/O happens here — call
    /// [`Layout::ensure_base`] to materialize the tree.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Locate the user's `~/.cln/` from `$HOME`. Returns `None` if HOME isn't
    /// set — the CLI turns that into a diagnostic; the library does not panic.
    pub fn from_home() -> Option<Self> {
        let home = std::env::var_os("HOME")?;
        if home.is_empty() {
            return None;
        }
        Some(Self::new(PathBuf::from(home).join(".cln")))
    }

    /// The root of the layout — normally `~/.cln/`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `~/.cln/bin/` — where the `cln` binary lives after install.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// `~/.cln/versions/` — parent of all per-kind version directories.
    pub fn versions_root(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// `~/.cln/versions/<kind>/` — every installed version of a single kind.
    pub fn versions_dir(&self, kind: ToolchainKind) -> PathBuf {
        self.versions_root().join(kind.as_str())
    }

    /// `~/.cln/active/` — the parent of the per-kind symlinks.
    pub fn active_root(&self) -> PathBuf {
        self.root.join("active")
    }

    /// `~/.cln/active/<kind>` — the symlink that points at the currently
    /// active version's directory under `versions/<kind>/`.
    pub fn active_link(&self, kind: ToolchainKind) -> PathBuf {
        self.active_root().join(kind.as_str())
    }

    /// `~/.cln/cache/` — where downloads land before extraction.
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `~/.cln/cache/downloads/` — verified archive blobs, keyed by SHA-256.
    pub fn downloads_dir(&self) -> PathBuf {
        self.cache_dir().join("downloads")
    }

    /// `~/.cln/host-wit/` — cached host contracts, per Manager §00.2.
    ///
    /// Distinct from `~/.cln/wit-cache/`, which Manager §00.2 also lists and
    /// which holds WIT synthesized from library declarations. This directory
    /// holds only `host.wit` files published by hosts, one per
    /// `<host>@<version>.wit`, byte-identical to what the host published.
    pub fn host_wit_dir(&self) -> PathBuf {
        self.root.join("host-wit")
    }

    /// Create the base directories that every M0 command assumes exist:
    /// root, bin, versions/{compiler,framework,runtime}, active,
    /// cache/downloads, host-wit.
    ///
    /// Idempotent — safe to call on every CLI invocation.
    pub fn ensure_base(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.bin_dir())?;
        for k in ToolchainKind::ALL {
            std::fs::create_dir_all(self.versions_dir(k))?;
        }
        std::fs::create_dir_all(self.active_root())?;
        std::fs::create_dir_all(self.downloads_dir())?;
        std::fs::create_dir_all(self.host_wit_dir())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn paths_are_all_under_root() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        assert!(l.bin_dir().starts_with(tmp.path()));
        assert!(l.versions_root().starts_with(tmp.path()));
        assert!(l.active_root().starts_with(tmp.path()));
        assert!(l.cache_dir().starts_with(tmp.path()));
        assert!(l.host_wit_dir().starts_with(tmp.path()));
        for k in ToolchainKind::ALL {
            assert!(l.versions_dir(k).starts_with(tmp.path()));
            assert!(l.active_link(k).starts_with(tmp.path()));
        }
    }

    #[test]
    fn ensure_base_creates_expected_tree() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path().join(".cln"));
        l.ensure_base().unwrap();

        assert!(l.root().is_dir());
        assert!(l.bin_dir().is_dir());
        assert!(l.active_root().is_dir());
        assert!(l.downloads_dir().is_dir());
        assert!(l.host_wit_dir().is_dir());
        for k in ToolchainKind::ALL {
            assert!(l.versions_dir(k).is_dir(), "versions/{} should exist", k);
        }
    }

    #[test]
    fn ensure_base_is_idempotent() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path().join(".cln"));
        l.ensure_base().unwrap();
        l.ensure_base().unwrap();
        assert!(l.root().is_dir());
    }
}
