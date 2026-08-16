//! Reading a package without running it — the data behind `cln inspect` and
//! behind the window a double-click opens (Manager §00.12, P-2).
//!
//! **Inspecting must never execute.** The whole point of showing a package
//! before acting on it is that the file may have arrived by email or download,
//! and the person opening it wants to know what it is before it runs. So this
//! module extracts and parses; it never spawns a runtime, and it never needs
//! one installed.

use std::path::{Path, PathBuf};

use cln_layout::Layout;
use cln_shared::ToolchainKind;

use crate::manifest::Kind;
use crate::{extract, run_cache, Artifact, RunError};

/// What a package says about itself, plus what this machine can do about it.
#[derive(Clone, Debug)]
pub struct Inspection {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    /// `clapp` (runs) or `serve` (deploys, or runs locally).
    pub kind: Kind,
    /// Every world the artifact declares.
    pub worlds: Vec<String>,
    /// The runtime the artifact pins, when it pins one at all.
    pub runtime_pin: Option<semver::Version>,
    /// The runtime that would actually be used, and whether it is installed.
    pub runtime_resolved: Option<semver::Version>,
    pub runtime_installed: bool,
    /// True when the archive carries a detached signature.
    ///
    /// Presence only — verification is a separate concern (§00.11.2) and
    /// claiming a signature is *valid* on the strength of a file existing
    /// would be worse than saying nothing.
    pub signed: bool,
    pub path: PathBuf,
}

impl Inspection {
    /// A human label for the kind, as the window shows it.
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            Kind::Clapp => "Application",
            Kind::Serve => "Server application",
        }
    }

    /// Whether this package's primary action is deploying rather than running.
    pub fn is_server(&self) -> bool {
        matches!(self.kind, Kind::Serve)
    }
}

/// Read a package and report what it is.
///
/// Extraction is shared with the run path, so a subsequent run of the same
/// archive reuses the cache rather than unpacking twice.
pub fn inspect(path: &Path, layout: &Layout) -> Result<Inspection, RunError> {
    let bundle = match crate::detect(path)? {
        Artifact::Bundle(b) => b,
        Artifact::Wasm(p) => return Err(RunError::NotAPackage { path: p }),
        Artifact::Project(p) => return Err(RunError::ProjectDirectory { path: p }),
    };

    let extracted = extract(&bundle, &run_cache(layout))?;
    let m = &extracted.manifest;

    let runtime_pin = m.runtime_pin();

    // What would actually run: the pin if there is one, else whatever is
    // active. Reported separately from the pin so the window can say "needs
    // 1.1.0, not installed" rather than silently falling back.
    let active = layout.active_version(ToolchainKind::Runtime);
    let runtime_resolved = runtime_pin.clone().or_else(|| active.clone());
    let runtime_installed = runtime_resolved
        .as_ref()
        .is_some_and(|v| layout.is_installed(ToolchainKind::Runtime, v));

    Ok(Inspection {
        name: m.package.name.clone(),
        version: m.package.version.clone(),
        description: m.package.description.clone(),
        kind: m.artifact.kind,
        worlds: m.artifact.worlds.clone(),
        runtime_pin,
        runtime_resolved,
        runtime_installed,
        signed: extracted.join("signature.sig").is_file(),
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::FileOptions::default();
            for (name, bytes) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(bytes).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn layout_with_runtime(versions: &[&str], active: Option<&str>) -> (tempfile::TempDir, Layout) {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        for s in versions {
            let v: semver::Version = s.parse().unwrap();
            std::fs::create_dir_all(layout.version_dir(ToolchainKind::Runtime, &v)).unwrap();
            std::fs::write(layout.version_binary(ToolchainKind::Runtime, &v), b"stub").unwrap();
        }
        if let Some(a) = active {
            layout
                .set_active(ToolchainKind::Runtime, &a.parse().unwrap())
                .unwrap();
        }
        (home, layout)
    }

    fn clapp(dir: &Path, kind: &str, runtime: &str, extra: &[(&str, &[u8])]) -> PathBuf {
        let manifest = format!(
            r#"
spec_version = "1"
[package]
name = "demo"
version = "1.2.0"
description = "A demo package"
[build]
runtime_version = "{runtime}"
[artifact]
kind = "{kind}"
worlds = ["cli"]
entry_wasm = "app.wasm"
[artifact.entries]
server = "wasm/server.wasm"
"#
        );
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("manifest.toml", manifest.as_bytes()),
            ("app.wasm", b"\0asm\x0d\x00\x01\x00"),
            ("config/host.toml", b"[guest]\n"),
        ];
        entries.extend_from_slice(extra);
        let p = dir.join("demo.clapp");
        std::fs::write(&p, zip_bytes(&entries)).unwrap();
        p
    }

    #[test]
    fn reports_what_the_package_says_about_itself() {
        let tmp = tempdir().unwrap();
        let (_h, l) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));
        let p = clapp(tmp.path(), "clapp", "unknown", &[]);

        let i = inspect(&p, &l).unwrap();
        assert_eq!(i.name, "demo");
        assert_eq!(i.version, "1.2.0");
        assert_eq!(i.description.as_deref(), Some("A demo package"));
        assert_eq!(i.kind_label(), "Application");
        assert!(!i.is_server());
        assert!(!i.signed);
    }

    /// The distinction the open window branches on, carried by `kind` rather
    /// than by a second file extension (§00.14, P-1).
    #[test]
    fn a_server_bundle_is_reported_as_one() {
        let tmp = tempdir().unwrap();
        let (_h, l) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));
        let p = clapp(tmp.path(), "serve", "unknown", &[]);

        let i = inspect(&p, &l).unwrap();
        assert!(i.is_server());
        assert_eq!(i.kind_label(), "Server application");
    }

    /// P-2: a missing pinned runtime is surfaced in the window rather than
    /// discovered when the run fails.
    #[test]
    fn a_pin_that_is_not_installed_is_reported_before_running() {
        let tmp = tempdir().unwrap();
        let (_h, l) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));
        let p = clapp(tmp.path(), "clapp", "9.9.9", &[]);

        let i = inspect(&p, &l).unwrap();
        assert_eq!(i.runtime_pin, Some(semver::Version::new(9, 9, 9)));
        assert!(!i.runtime_installed, "9.9.9 is not installed");
    }

    #[test]
    fn an_unpinned_package_resolves_to_the_active_runtime() {
        let tmp = tempdir().unwrap();
        let (_h, l) = layout_with_runtime(&["1.4.2"], Some("1.4.2"));
        let p = clapp(tmp.path(), "clapp", "unknown", &[]);

        let i = inspect(&p, &l).unwrap();
        assert!(i.runtime_pin.is_none());
        assert_eq!(i.runtime_resolved, Some(semver::Version::new(1, 4, 2)));
        assert!(i.runtime_installed);
    }

    /// Presence is reported; validity deliberately is not.
    #[test]
    fn a_carried_signature_is_reported_as_present() {
        let tmp = tempdir().unwrap();
        let (_h, l) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));
        let p = clapp(tmp.path(), "clapp", "unknown", &[("signature.sig", b"sig")]);

        let i = inspect(&p, &l).unwrap();
        assert!(i.signed);
    }

    /// Inspecting must not require a runtime: the point is to look before
    /// installing anything.
    #[test]
    fn inspecting_works_with_no_runtime_installed() {
        let tmp = tempdir().unwrap();
        let (_h, l) = layout_with_runtime(&[], None);
        let p = clapp(tmp.path(), "clapp", "unknown", &[]);

        let i = inspect(&p, &l).unwrap();
        assert_eq!(i.name, "demo");
        assert!(!i.runtime_installed);
        assert!(i.runtime_resolved.is_none());
    }

    #[test]
    fn a_bare_wasm_is_not_a_package() {
        let tmp = tempdir().unwrap();
        let (_h, l) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));
        let w = tmp.path().join("app.wasm");
        std::fs::write(&w, b"\0asm\x0d\x00\x01\x00").unwrap();

        let err = inspect(&w, &l).unwrap_err();
        assert!(matches!(err, RunError::NotAPackage { .. }));
    }
}
