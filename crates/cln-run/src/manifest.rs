//! Reading `manifest.toml` out of a bundle (Manager §00.14).
//!
//! **This is the read half of a format the framework writes.** The producer
//! side lives in `framework-package::manifest`; this type deserializes the
//! same file. It is deliberately *not* a copy of the producer's struct:
//!
//! - Manager reads far fewer fields than the framework writes. `cln run` needs
//!   the world, the entry wasm, and the runtime pin. Everything else —
//!   authors, license, bridge versions — is inspected by `cln inspect` or by
//!   Cloud, not by dispatch.
//! - Unknown fields are *tolerated*. A bundle produced by a newer framework
//!   must still run under an older manager as long as `spec_version` matches,
//!   so this parses permissively rather than rejecting on an unrecognized key.
//!
//! What is *not* tolerated is a `spec_version` this manager does not know:
//! that is the one field whose whole job is to say "the layout changed", and
//! guessing past it would mean resolving paths that moved.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The manifest layout version this manager understands (Manager §00.14,
/// "Format ownership"). A bump here is a spec change, not a refactor.
pub const SUPPORTED_SPEC_VERSION: &str = "1";

/// The file name at the archive root.
pub const MANIFEST_NAME: &str = "manifest.toml";

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("{MANIFEST_NAME} is not valid TOML: {source}")]
    Malformed {
        #[source]
        source: toml::de::Error,
    },

    #[error("this bundle declares manifest spec_version '{found}', but this `cln` understands '{SUPPORTED_SPEC_VERSION}'")]
    UnsupportedSpecVersion { found: String },

    #[error("the manifest declares no world; `cln run` cannot pick a host without one")]
    NoWorld,

    #[error("a clapp manifest must name its entry_wasm")]
    NoEntryWasm,

    #[error("the manifest declares world '{world}' but lists no entry for it")]
    NoEntryForWorld { world: String },
}

impl ManifestError {
    pub fn remedy(&self) -> Option<String> {
        match self {
            ManifestError::UnsupportedSpecVersion { .. } => {
                Some("this bundle was built by a newer toolchain; run `cln self-update`".into())
            }
            // Every other variant means the producer wrote something invalid.
            // There is no user-side fix, so pointing at one would mislead.
            _ => None,
        }
    }
}

/// What kind of artifact a bundle holds.
///
/// **The extension does not carry this.** `framework-package::file_name`
/// always emits `.clapp`, for a server bundle as much as an application, so
/// `kind` here is the only discriminator — which is exactly why §00.13 has
/// `cln run` read the manifest before it touches any wasm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Clapp,
    Serve,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Clapp => "clapp",
            Kind::Serve => "serve",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub spec_version: String,
    pub package: Package,
    #[serde(default)]
    pub build: Build,
    pub artifact: Artifact,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Provenance stamped at package time.
///
/// Every field defaults: a manifest that omits the block entirely still
/// parses, because none of these are load-bearing for dispatch. The one that
/// matters, `runtime_version`, is interpreted by [`Manifest::runtime_pin`]
/// rather than trusted as written — see that method for why.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Build {
    #[serde(default)]
    pub compiler_version: String,
    #[serde(default)]
    pub framework_version: String,
    #[serde(default)]
    pub runtime_version: String,
    #[serde(default)]
    pub built_at: String,
    #[serde(default)]
    pub built_by: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Artifact {
    pub kind: Kind,
    #[serde(default)]
    pub worlds: Vec<String>,
    /// `clapp` only — the single component to run.
    #[serde(default)]
    pub entry_wasm: Option<String>,
    /// `serve` only — world name to archive-relative wasm path.
    #[serde(default)]
    pub entries: BTreeMap<String, String>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest =
            toml::from_str(text).map_err(|source| ManifestError::Malformed { source })?;

        if manifest.spec_version != SUPPORTED_SPEC_VERSION {
            return Err(ManifestError::UnsupportedSpecVersion {
                found: manifest.spec_version,
            });
        }

        Ok(manifest)
    }

    /// The world `cln run` should ask the runtime for, and the archive-relative
    /// wasm that world runs.
    ///
    /// A `clapp` declares exactly one world and one `entry_wasm`. A `serve`
    /// bundle may declare several, so a caller that wants a specific one has to
    /// say which; this picks the first declared world as the default, which is
    /// the only defensible choice when the user named no preference.
    pub fn entry(&self, requested_world: Option<&str>) -> Result<Entry, ManifestError> {
        let world = match requested_world {
            Some(w) => w.to_string(),
            None => self
                .artifact
                .worlds
                .first()
                .cloned()
                .ok_or(ManifestError::NoWorld)?,
        };

        let wasm = match self.artifact.kind {
            // `entry_wasm` is the single component; the world selects the host,
            // not a different file.
            Kind::Clapp => self
                .artifact
                .entry_wasm
                .clone()
                .ok_or(ManifestError::NoEntryWasm)?,
            Kind::Serve => self.artifact.entries.get(&world).cloned().ok_or_else(|| {
                ManifestError::NoEntryForWorld {
                    world: world.clone(),
                }
            })?,
        };

        Ok(Entry { world, wasm })
    }

    /// The runtime version this artifact pins, if it pins one at all.
    ///
    /// **Why this is not just `build.runtime_version`.** §00.13 calls the field
    /// an exact pin that MUST be installed, but the framework stamps the
    /// literal string `"unknown"` when it has no runtime handle to read a
    /// version from — which is every artifact it produces today. Treating an
    /// unparseable value as a pin would make those bundles permanently
    /// unrunnable, and treating it as version `0.0.0` would resolve to a
    /// runtime nobody installed.
    ///
    /// So: a value that parses as semver is a real pin and binds strictly. A
    /// value that does not is the producer declining to pin, and resolution
    /// falls through to the project pin and then the global active runtime.
    /// The rule stays exactly as strict as §00.13 wherever a pin actually
    /// exists.
    pub fn runtime_pin(&self) -> Option<semver::Version> {
        semver::Version::parse(self.build.runtime_version.trim()).ok()
    }
}

/// The pair `cln run` needs to invoke the runtime: which host, which component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub world: String,
    /// Archive-relative, e.g. `app.wasm` or `wasm/server.wasm`.
    pub wasm: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A byte-for-byte copy of the manifest inside a `.clapp` produced by
    /// `clean-framework` 0.1.1. Pinned here so a producer-side change that
    /// breaks this reader fails in manager's own suite.
    const REAL_CLAPP_MANIFEST: &str = r#"
spec_version = "1"

[package]
name = "hello-world"
version = "0.1.0"

[build]
compiler_version = "0.0.0-fake"
framework_version = "0.1.1"
runtime_version = "unknown"
built_at = "2026-08-14T12:00:00Z"
built_by = "clean-framework 0.1.1"

[artifact]
kind = "clapp"
worlds = ["cli"]
entry_wasm = "app.wasm"

[integrity.wasm_sha256]
"app.wasm" = "9d4c5e70cad9b3c9ba863aeec54593438c3ab904a17fbcd0de71c7792b20d53c"
"#;

    #[test]
    fn parses_a_real_framework_produced_manifest() {
        let m = Manifest::parse(REAL_CLAPP_MANIFEST).unwrap();
        assert_eq!(m.package.name, "hello-world");
        assert_eq!(m.artifact.kind, Kind::Clapp);

        let entry = m.entry(None).unwrap();
        assert_eq!(entry.world, "cli");
        assert_eq!(entry.wasm, "app.wasm");
    }

    /// The `[integrity]` table above is not modeled by this reader at all.
    /// Parsing must ignore it rather than fail, or every real bundle breaks.
    #[test]
    fn unmodeled_tables_are_tolerated() {
        let m = Manifest::parse(REAL_CLAPP_MANIFEST);
        assert!(m.is_ok(), "unknown fields must not fail the parse");
    }

    /// The decision recorded in `runtime_pin`: `"unknown"` is the framework
    /// declining to pin, not a pin that can never be satisfied.
    #[test]
    fn unknown_runtime_version_is_not_a_pin() {
        let m = Manifest::parse(REAL_CLAPP_MANIFEST).unwrap();
        assert_eq!(m.runtime_pin(), None);
    }

    #[test]
    fn a_semver_runtime_version_is_a_pin() {
        let text = REAL_CLAPP_MANIFEST.replace(
            r#"runtime_version = "unknown""#,
            r#"runtime_version = "1.2.3""#,
        );
        let m = Manifest::parse(&text).unwrap();
        assert_eq!(m.runtime_pin(), Some(semver::Version::new(1, 2, 3)));
    }

    /// A future layout change bumps `spec_version`; resolving paths under the
    /// old assumptions would read files that moved.
    #[test]
    fn a_future_spec_version_is_refused_with_a_remedy() {
        let text = REAL_CLAPP_MANIFEST.replace(r#"spec_version = "1""#, r#"spec_version = "2""#);
        let err = Manifest::parse(&text).unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedSpecVersion { .. }));
        assert!(err.remedy().unwrap().contains("self-update"));
    }

    #[test]
    fn malformed_toml_is_reported_as_such() {
        let err = Manifest::parse("this is not toml {{{").unwrap_err();
        assert!(matches!(err, ManifestError::Malformed { .. }));
    }

    /// `.serve` bundles route the world to a different component; a `.clapp`
    /// has one component regardless of world.
    #[test]
    fn a_serve_bundle_selects_its_component_by_world() {
        let text = r#"
spec_version = "1"
[package]
name = "svc"
version = "1.0.0"
[artifact]
kind = "serve"
worlds = ["server", "worker"]
[artifact.entries]
server = "wasm/server.wasm"
worker = "wasm/worker.wasm"
"#;
        let m = Manifest::parse(text).unwrap();

        // No preference: the first declared world wins.
        assert_eq!(m.entry(None).unwrap().wasm, "wasm/server.wasm");
        assert_eq!(m.entry(Some("worker")).unwrap().wasm, "wasm/worker.wasm");
    }

    #[test]
    fn asking_a_serve_bundle_for_a_world_it_lacks_names_the_world() {
        let text = r#"
spec_version = "1"
[package]
name = "svc"
version = "1.0.0"
[artifact]
kind = "serve"
worlds = ["server"]
[artifact.entries]
server = "wasm/server.wasm"
"#;
        let m = Manifest::parse(text).unwrap();
        let err = m.entry(Some("worker")).unwrap_err();
        assert!(matches!(err, ManifestError::NoEntryForWorld { .. }));
        assert!(err.to_string().contains("worker"));
    }

    /// A `.clapp` runs its one component whichever world is named, because the
    /// world picks the host, not the file.
    #[test]
    fn a_clapp_ignores_the_world_when_choosing_its_component() {
        let m = Manifest::parse(REAL_CLAPP_MANIFEST).unwrap();
        assert_eq!(m.entry(Some("cli")).unwrap().wasm, "app.wasm");
    }

    #[test]
    fn a_manifest_with_no_worlds_cannot_pick_a_host() {
        let text = r#"
spec_version = "1"
[package]
name = "x"
version = "1.0.0"
[artifact]
kind = "clapp"
worlds = []
entry_wasm = "app.wasm"
"#;
        let m = Manifest::parse(text).unwrap();
        assert!(matches!(m.entry(None), Err(ManifestError::NoWorld)));
    }

    /// The `[build]` block carries provenance, not dispatch inputs. A bundle
    /// that omits it entirely must still run.
    #[test]
    fn a_missing_build_block_still_parses() {
        let text = r#"
spec_version = "1"
[package]
name = "x"
version = "1.0.0"
[artifact]
kind = "clapp"
worlds = ["cli"]
entry_wasm = "app.wasm"
"#;
        let m = Manifest::parse(text).unwrap();
        assert_eq!(m.runtime_pin(), None);
        assert_eq!(m.entry(None).unwrap().world, "cli");
    }
}
