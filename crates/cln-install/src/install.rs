//! End-to-end install orchestration: resolve → download → extract → activate.

use cln_layout::{active::ActivateError, Layout};
use cln_shared::{Platform, ToolchainKind};

use crate::channels::{ChannelError, ReleaseSource, VersionSpec};
use crate::download::{fetch, DownloadError};
use crate::extract::{extract_archive, ExtractError};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Channel(#[from] ChannelError),
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Activate(#[from] ActivateError),
    #[error("io error preparing install: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InstallOutcome {
    pub kind: ToolchainKind,
    pub version: semver::Version,
    /// True when the requested version was already installed and no work
    /// beyond activation was performed.
    pub already_installed: bool,
    /// True when the active symlink was pointed at this version.
    pub activated: bool,
}

/// Install `spec` for the given `source`. Idempotent: if the version is
/// already installed, we skip download+extract. When `activate` is true, the
/// active symlink is pointed at the resulting version.
pub fn install(
    layout: &Layout,
    source: &dyn ReleaseSource,
    spec: &VersionSpec,
    platform: Platform,
    activate: bool,
) -> Result<InstallOutcome, InstallError> {
    layout.ensure_base()?;

    let entry = source.resolve(spec, platform)?;
    let kind = entry.kind;
    let version = entry.version.clone();
    let dest = layout.version_dir(kind, &version);
    let already = layout.is_installed(kind, &version);

    if !already {
        let archive = fetch(layout, &entry)?;
        extract_archive(&archive, &dest, &entry.asset_url, kind.binary_name())?;
    }

    let mut activated = false;
    if activate {
        layout.set_active(kind, &version)?;
        activated = true;
    }

    Ok(InstallOutcome {
        kind,
        version,
        already_installed: already,
        activated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::LocalDir;
    use cln_shared::platform::{Arch, Os};
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn plat() -> Platform {
        Platform {
            os: Os::Macos,
            arch: Arch::Arm64,
        }
    }

    fn seed_release_dir(
        root: &Path,
        kind: ToolchainKind,
        tag: &str,
        platform: Platform,
        binary_bytes: &[u8],
    ) {
        let dir = root.join(tag);
        fs::create_dir_all(&dir).unwrap();
        let asset_name = format!(
            "{}-{}-{}.{}",
            kind.binary_name(),
            tag,
            platform,
            platform.archive_ext()
        );
        let archive_path = dir.join(&asset_name);

        let f = fs::File::create(&archive_path).unwrap();
        let gz = GzEncoder::new(f, Compression::default());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary_bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, kind.binary_name(), binary_bytes)
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let sha = hex(Sha256::digest(fs::read(&archive_path).unwrap()).as_slice());
        fs::write(
            dir.join(format!("{asset_name}.sha256")),
            format!("{sha}  {asset_name}\n"),
        )
        .unwrap();
    }

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    #[test]
    fn install_end_to_end_places_binary_and_activates() {
        let releases = tempdir().unwrap();
        seed_release_dir(
            releases.path(),
            ToolchainKind::Compiler,
            "1.0.0",
            plat(),
            b"compiler-1.0.0",
        );

        let home = tempdir().unwrap();
        let layout = Layout::new(home.path().join(".cln"));
        let source = LocalDir::new(ToolchainKind::Compiler, releases.path());

        let outcome = install(&layout, &source, &VersionSpec::Latest, plat(), true).unwrap();
        assert_eq!(outcome.kind, ToolchainKind::Compiler);
        assert_eq!(outcome.version, "1.0.0".parse().unwrap());
        assert!(!outcome.already_installed);
        assert!(outcome.activated);

        // Binary is on disk.
        let bin = layout.version_binary(ToolchainKind::Compiler, &outcome.version);
        assert_eq!(fs::read(&bin).unwrap(), b"compiler-1.0.0");

        // Active symlink resolves to the version dir.
        let active = layout.active_link(ToolchainKind::Compiler);
        let resolved = fs::read_link(&active).unwrap();
        assert_eq!(
            resolved,
            layout.version_dir(ToolchainKind::Compiler, &outcome.version)
        );
    }

    #[test]
    fn install_is_idempotent_when_already_present() {
        let releases = tempdir().unwrap();
        seed_release_dir(
            releases.path(),
            ToolchainKind::Runtime,
            "0.1.0",
            plat(),
            b"runtime-0.1.0",
        );

        let home = tempdir().unwrap();
        let layout = Layout::new(home.path().join(".cln"));
        let source = LocalDir::new(ToolchainKind::Runtime, releases.path());

        let first = install(&layout, &source, &VersionSpec::Latest, plat(), true).unwrap();
        assert!(!first.already_installed);
        let second = install(&layout, &source, &VersionSpec::Latest, plat(), true).unwrap();
        assert!(second.already_installed);
        assert_eq!(first.version, second.version);
    }

    #[test]
    fn install_all_three_kinds_creates_three_symlinks() {
        let releases_root = tempdir().unwrap();
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path().join(".cln"));

        for kind in ToolchainKind::ALL {
            let sub = releases_root.path().join(kind.as_str());
            fs::create_dir_all(&sub).unwrap();
            seed_release_dir(
                &sub,
                kind,
                "1.0.0",
                plat(),
                format!("{kind}-1.0.0").as_bytes(),
            );
            let source = LocalDir::new(kind, &sub);
            let outcome = install(&layout, &source, &VersionSpec::Latest, plat(), true).unwrap();
            assert!(outcome.activated);
        }

        for kind in ToolchainKind::ALL {
            let v = layout.active_version(kind).unwrap();
            assert_eq!(
                v,
                "1.0.0".parse().unwrap(),
                "kind {kind} should be active at 1.0.0"
            );
        }
    }
}
