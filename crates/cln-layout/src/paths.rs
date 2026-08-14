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

    /// Locate the toolchain root: `$CLN_HOME` when set, otherwise `~/.cln/`.
    /// Returns `None` when neither variable is usable — the CLI turns that into
    /// a diagnostic; the library does not panic.
    ///
    /// **`CLN_HOME` is the layout root itself, not a home directory.** It is
    /// used verbatim, with no `.cln` appended, so `CLN_HOME=/srv/toolchain`
    /// puts versions at `/srv/toolchain/versions/`. That matches how the CLI
    /// has always treated the variable.
    ///
    /// The override lives here rather than in `cln-cli` because every process
    /// that resolves a toolchain has to agree on the answer. The framework
    /// calls this function directly to find the compiler a project pins, so an
    /// override applied only in the CLI would be invisible to the build it
    /// dispatched — the toolchain would resolve from two different roots in one
    /// command. Operators provisioning a toolchain outside a service account's
    /// home (Clean Cloud builds projects this way) depend on the whole chain
    /// honoring one root.
    pub fn from_home() -> Option<Self> {
        if let Some(root) = std::env::var_os("CLN_HOME").filter(|v| !v.is_empty()) {
            return Some(Self::new(PathBuf::from(root)));
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
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

    /// `from_home` reads process-global environment, so the tests that set
    /// `CLN_HOME` and `HOME` have to take turns. Cargo runs tests in threads of
    /// one process, so without this they race and read each other's values.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `CLN_HOME` and `HOME` set to the given values, restoring
    /// whatever was there before — including for a panicking `f`, since the
    /// lock would otherwise stay poisoned for every later test.
    fn with_env<T>(cln_home: Option<&Path>, home: Option<&Path>, f: impl FnOnce() -> T) -> T {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let prev_cln = std::env::var_os("CLN_HOME");
        let prev_home = std::env::var_os("HOME");

        // SAFETY: the lock above serializes every writer in this module, and
        // these tests are the only ones in the crate that touch these vars.
        unsafe {
            apply("CLN_HOME", cln_home);
            apply("HOME", home);
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

        unsafe {
            restore("CLN_HOME", prev_cln);
            restore("HOME", prev_home);
        }
        drop(guard);

        match result {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    unsafe fn apply(key: &str, value: Option<&Path>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    unsafe fn restore(key: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// The regression test for the cross-component bug: the framework resolves
    /// its compiler through `from_home`, so an override honored only in the CLI
    /// left a dispatched build reading a different root than the command that
    /// spawned it.
    #[test]
    fn cln_home_overrides_the_home_directory() {
        let cln_home = tempdir().unwrap();
        let home = tempdir().unwrap();

        let layout = with_env(Some(cln_home.path()), Some(home.path()), || {
            Layout::from_home().expect("CLN_HOME should resolve a layout")
        });

        assert_eq!(layout.root(), cln_home.path());
        assert_ne!(layout.root(), home.path().join(".cln"));
    }

    /// `CLN_HOME` names the layout root itself — no `.cln` is appended, so it
    /// agrees with `Layout::new` and with how the CLI already treated it.
    #[test]
    fn cln_home_is_the_root_itself_not_a_home_directory() {
        let cln_home = tempdir().unwrap();

        let layout = with_env(Some(cln_home.path()), None, || Layout::from_home().unwrap());

        assert_eq!(layout.root(), cln_home.path());
        assert_eq!(
            layout.versions_root(),
            cln_home.path().join("versions"),
            "versions must sit directly under CLN_HOME"
        );
    }

    #[test]
    fn falls_back_to_home_when_cln_home_is_unset() {
        let home = tempdir().unwrap();

        let layout = with_env(None, Some(home.path()), || Layout::from_home().unwrap());

        assert_eq!(layout.root(), home.path().join(".cln"));
    }

    /// An empty value is treated as unset rather than as the root `""`, which
    /// would otherwise resolve every path to a relative one.
    #[test]
    fn an_empty_cln_home_falls_back_to_home() {
        let home = tempdir().unwrap();

        let layout = with_env(Some(Path::new("")), Some(home.path()), || {
            Layout::from_home().unwrap()
        });

        assert_eq!(layout.root(), home.path().join(".cln"));
    }

    #[test]
    fn no_cln_home_and_no_home_resolves_to_nothing() {
        assert!(with_env(None, None, Layout::from_home).is_none());
    }

    #[test]
    fn an_empty_home_is_treated_as_unset() {
        assert!(with_env(None, Some(Path::new("")), Layout::from_home).is_none());
    }

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
