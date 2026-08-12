//! Managing installed versions of one toolchain kind under
//! `~/.cln/versions/<kind>/<version>/`.

use std::io;
use std::path::PathBuf;

use cln_shared::ToolchainKind;
use semver::Version;

use crate::paths::Layout;

impl Layout {
    /// `~/.cln/versions/<kind>/<version>/` — the install directory for one
    /// specific release. Does NOT check whether it exists.
    pub fn version_dir(&self, kind: ToolchainKind, version: &Version) -> PathBuf {
        self.versions_dir(kind).join(version.to_string())
    }

    /// The path where the extracted binary is expected to land inside a
    /// version directory. Matches Manager §00.2:
    /// `~/.cln/versions/compiler/1.4.0/clean-compiler`.
    pub fn version_binary(&self, kind: ToolchainKind, version: &Version) -> PathBuf {
        let name = match std::env::consts::OS {
            "windows" => format!("{}.exe", kind.binary_name()),
            _ => kind.binary_name().to_string(),
        };
        self.version_dir(kind, version).join(name)
    }

    /// True when the version directory exists and contains the expected binary.
    /// Nothing tries to run the binary here; that's the caller's job.
    pub fn is_installed(&self, kind: ToolchainKind, version: &Version) -> bool {
        self.version_binary(kind, version).is_file()
    }

    /// Every installed version of one kind, sorted ascending. Ignores entries
    /// that don't parse as semver — those are treated as foreign, not errors.
    pub fn list_installed(&self, kind: ToolchainKind) -> io::Result<Vec<Version>> {
        let dir = self.versions_dir(kind);
        let mut out = Vec::new();
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in read {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Ok(v) = Version::parse(name) {
                out.push(v);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Remove an installed version's directory. Errors if it doesn't exist.
    ///
    /// This does NOT check whether the version is currently active —
    /// `cln-install::uninstall` is responsible for that policy (Manager §00.3.3
    /// says `cln uninstall` MUST fail on the active version). Keeping the
    /// policy out of layout lets tests exercise removal freely.
    pub fn remove_version(&self, kind: ToolchainKind, version: &Version) -> io::Result<()> {
        std::fs::remove_dir_all(self.version_dir(kind, version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn list_installed_empty_when_no_dir() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        // Not even ensure_base — should return empty, not error.
        assert!(l
            .list_installed(ToolchainKind::Compiler)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn list_installed_returns_sorted_semver_dirs_only() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();

        for v in ["1.0.0", "0.2.1", "2.0.0-rc.1", "not-semver"] {
            std::fs::create_dir_all(l.versions_dir(ToolchainKind::Compiler).join(v)).unwrap();
        }

        let installed = l.list_installed(ToolchainKind::Compiler).unwrap();
        let strs: Vec<String> = installed.iter().map(|v| v.to_string()).collect();
        assert_eq!(strs, vec!["0.2.1", "1.0.0", "2.0.0-rc.1"]);
    }

    #[test]
    fn is_installed_detects_binary_file() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();

        let v: Version = "1.0.0".parse().unwrap();
        assert!(!l.is_installed(ToolchainKind::Compiler, &v));

        std::fs::create_dir_all(l.version_dir(ToolchainKind::Compiler, &v)).unwrap();
        std::fs::write(l.version_binary(ToolchainKind::Compiler, &v), b"stub").unwrap();
        assert!(l.is_installed(ToolchainKind::Compiler, &v));
    }

    #[test]
    fn remove_version_deletes_tree() {
        let tmp = tempdir().unwrap();
        let l = Layout::new(tmp.path());
        l.ensure_base().unwrap();

        let v: Version = "1.0.0".parse().unwrap();
        std::fs::create_dir_all(l.version_dir(ToolchainKind::Runtime, &v)).unwrap();
        std::fs::write(l.version_binary(ToolchainKind::Runtime, &v), b"stub").unwrap();

        l.remove_version(ToolchainKind::Runtime, &v).unwrap();
        assert!(!l.version_dir(ToolchainKind::Runtime, &v).exists());
    }
}
