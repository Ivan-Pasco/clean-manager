//! Remove an installed toolchain version.
//!
//! Enforces Manager §00.3.3: `cln uninstall` MUST refuse to remove the
//! currently active version. This policy lives here (not in `cln-layout`) so
//! that the layout crate remains pure mechanism.

use cln_layout::Layout;
use cln_shared::ToolchainKind;
use semver::Version;

#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    #[error("version {version} of {kind} is not installed")]
    NotInstalled {
        kind: ToolchainKind,
        version: Version,
    },
    #[error("cannot uninstall the currently active {kind} version {version}; switch first with `cln use {kind} <other>`")]
    IsActive {
        kind: ToolchainKind,
        version: Version,
    },
    #[error("io error while removing version: {0}")]
    Io(#[from] std::io::Error),
}

pub fn uninstall(
    layout: &Layout,
    kind: ToolchainKind,
    version: &Version,
) -> Result<(), UninstallError> {
    if !layout.version_dir(kind, version).exists() {
        return Err(UninstallError::NotInstalled {
            kind,
            version: version.clone(),
        });
    }
    if layout.active_version(kind).as_ref() == Some(version) {
        return Err(UninstallError::IsActive {
            kind,
            version: version.clone(),
        });
    }
    layout.remove_version(kind, version)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn install_stub(layout: &Layout, kind: ToolchainKind, version: &str) -> Version {
        let v: Version = version.parse().unwrap();
        std::fs::create_dir_all(layout.version_dir(kind, &v)).unwrap();
        std::fs::write(layout.version_binary(kind, &v), b"stub").unwrap();
        v
    }

    #[test]
    fn refuses_to_remove_active_version() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(tmp.path());
        layout.ensure_base().unwrap();

        let v = install_stub(&layout, ToolchainKind::Compiler, "1.0.0");
        layout.set_active(ToolchainKind::Compiler, &v).unwrap();

        let err = uninstall(&layout, ToolchainKind::Compiler, &v).unwrap_err();
        assert!(matches!(err, UninstallError::IsActive { .. }));
        assert!(layout.version_dir(ToolchainKind::Compiler, &v).is_dir());
    }

    #[test]
    fn removes_non_active_version() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(tmp.path());
        layout.ensure_base().unwrap();

        let v1 = install_stub(&layout, ToolchainKind::Framework, "1.0.0");
        let v2 = install_stub(&layout, ToolchainKind::Framework, "1.1.0");
        layout.set_active(ToolchainKind::Framework, &v2).unwrap();

        uninstall(&layout, ToolchainKind::Framework, &v1).unwrap();
        assert!(!layout.version_dir(ToolchainKind::Framework, &v1).exists());
        // Active is untouched.
        assert_eq!(layout.active_version(ToolchainKind::Framework), Some(v2));
    }

    #[test]
    fn missing_version_errors() {
        let tmp = tempdir().unwrap();
        let layout = Layout::new(tmp.path());
        layout.ensure_base().unwrap();

        let v: Version = "9.9.9".parse().unwrap();
        let err = uninstall(&layout, ToolchainKind::Runtime, &v).unwrap_err();
        assert!(matches!(err, UninstallError::NotInstalled { .. }));
    }
}
