//! The `~/.cln/active/<kind>` symlinks that point at the currently active
//! version of each toolchain kind, per Manager §00.2.
//!
//! Switching a symlink is done as atomically as the OS permits. On Unix we
//! create a temporary sibling symlink and `rename(2)` it over the target —
//! POSIX guarantees this replaces the link atomically. On Windows there is no
//! equivalent primitive, so we remove-then-create and accept a brief window
//! in which no active link exists.

use std::io;
use std::path::{Path, PathBuf};

use cln_shared::ToolchainKind;
use semver::Version;

use crate::paths::Layout;

#[derive(Debug, thiserror::Error)]
pub enum ActivateError {
    #[error("version {version} of {kind} is not installed at {expected}")]
    NotInstalled { kind: ToolchainKind, version: Version, expected: PathBuf },
    #[error("io error while switching active {kind}: {source}")]
    Io { kind: ToolchainKind, #[source] source: io::Error },
}

impl Layout {
    /// Point `~/.cln/active/<kind>` at `~/.cln/versions/<kind>/<version>/`.
    /// Errors if the version isn't installed.
    pub fn set_active(&self, kind: ToolchainKind, version: &Version) -> Result<(), ActivateError> {
        let target = self.version_dir(kind, version);
        if !target.is_dir() {
            return Err(ActivateError::NotInstalled {
                kind,
                version: version.clone(),
                expected: target,
            });
        }
        let link = self.active_link(kind);
        atomic_symlink_swap(&target, &link)
            .map_err(|source| ActivateError::Io { kind, source })
    }

    /// The version currently pointed at by `active/<kind>`, or `None` if the
    /// link is missing, dangling, or points somewhere that doesn't parse as
    /// a version directory under `versions/<kind>/`.
    pub fn active_version(&self, kind: ToolchainKind) -> Option<Version> {
        let link = self.active_link(kind);
        let target = std::fs::read_link(&link).ok()?;
        // The link stores an absolute path (see atomic_symlink_swap).
        let file_name = target.file_name()?.to_str()?;
        Version::parse(file_name).ok()
    }
}

/// Replace `link_path` with a symlink pointing at `target`, atomically on Unix.
fn atomic_symlink_swap(target: &Path, link_path: &Path) -> io::Result<()> {
    // Ensure the parent of the link exists — callers normally ran ensure_base,
    // but tests may not.
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Build a unique temp path in the same directory so the rename stays
    // within one filesystem.
    let mut tmp = link_path.to_path_buf();
    let file_name = link_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("active");
    let pid = std::process::id();
    // Uniqueness within a process: nanoseconds since UNIX_EPOCH.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    tmp.set_file_name(format!(".{file_name}.tmp.{pid}.{nanos}"));

    // Clean up any leftover from a previous crashed run.
    let _ = std::fs::remove_file(&tmp);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &tmp)?;
        // rename(2) on Unix atomically replaces the destination symlink.
        std::fs::rename(&tmp, link_path)?;
    }

    #[cfg(windows)]
    {
        // On Windows we have to remove first — there's no atomic replace for
        // symlinks. The window is small; if a concurrent reader falls into it,
        // they'll see ENOENT and can retry.
        let _ = std::fs::remove_dir_all(link_path).or_else(|_| std::fs::remove_file(link_path));
        std::os::windows::fs::symlink_dir(target, link_path)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn install_stub(l: &Layout, kind: ToolchainKind, version: &str) -> Version {
        let v: Version = version.parse().unwrap();
        std::fs::create_dir_all(l.version_dir(kind, &v)).unwrap();
        std::fs::write(l.version_binary(kind, &v), b"stub").unwrap();
        v
    }

    #[test]
    fn set_active_errors_when_version_missing() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();

        let v: Version = "1.0.0".parse().unwrap();
        let err = l.set_active(ToolchainKind::Compiler, &v).unwrap_err();
        assert!(matches!(err, ActivateError::NotInstalled { .. }));
    }

    #[test]
    fn set_active_creates_symlink_to_version_dir() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();

        let v = install_stub(&l, ToolchainKind::Compiler, "1.0.0");
        l.set_active(ToolchainKind::Compiler, &v).unwrap();

        let link = l.active_link(ToolchainKind::Compiler);
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(resolved, l.version_dir(ToolchainKind::Compiler, &v));
    }

    #[test]
    fn set_active_can_switch_between_versions() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();

        let v1 = install_stub(&l, ToolchainKind::Framework, "1.0.0");
        let v2 = install_stub(&l, ToolchainKind::Framework, "1.1.0");

        l.set_active(ToolchainKind::Framework, &v1).unwrap();
        assert_eq!(l.active_version(ToolchainKind::Framework), Some(v1.clone()));

        l.set_active(ToolchainKind::Framework, &v2).unwrap();
        assert_eq!(l.active_version(ToolchainKind::Framework), Some(v2));
    }

    #[test]
    fn active_version_none_when_link_missing() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();
        assert_eq!(l.active_version(ToolchainKind::Runtime), None);
    }

    #[test]
    fn active_kinds_are_independent() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();

        let vc = install_stub(&l, ToolchainKind::Compiler, "1.0.0");
        let vf = install_stub(&l, ToolchainKind::Framework, "2.0.0");

        l.set_active(ToolchainKind::Compiler, &vc).unwrap();
        l.set_active(ToolchainKind::Framework, &vf).unwrap();

        assert_eq!(l.active_version(ToolchainKind::Compiler), Some(vc));
        assert_eq!(l.active_version(ToolchainKind::Framework), Some(vf));
        assert_eq!(l.active_version(ToolchainKind::Runtime), None);
    }
}
