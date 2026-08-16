//! `cln run <path>` — Manager §00.13.
//!
//! **Manager routes; it never executes.** No crate here links a wasm engine,
//! parses a component, or interprets a guest's output. The whole job is to
//! decide *which* runtime binary to spawn, *which* world to ask it for, and
//! *which* configuration to hand it — then get out of the way of the process
//! that does the work (Architecture Boundaries §2.3).
//!
//! The pipeline, mirroring §00.13's six steps:
//!
//! 1. [`artifact::detect`] — bundle, bare wasm, or project directory.
//! 2. [`extract`] — unpack a bundle into `~/.cln/cache/run/<sha>/`, structure
//!    preserved.
//! 3. [`manifest`] — read the world and entry component out of `manifest.toml`.
//! 4. [`runtime::resolve_runtime`] — artifact pin → project pin → active.
//! 5. [`invoke`] — spawn the runtime with §00.13's argv shape.
//! 6. The guest's streams and exit code reach the user untouched.
//!
//! # `cln run <project-dir>` is not implemented here
//!
//! §00.13 covers running a project directory by building it first. That path
//! needs framework dispatch, which would make this crate depend on the build
//! side of the toolchain to serve a case the user can already express as two
//! commands. It is deferred; [`run`] reports a project directory with the
//! `cln build` / `cln run` pair that does the same thing today.

pub mod artifact;
pub mod devconfig;
pub mod extract;
pub mod inspect;
pub mod invoke;
pub mod manifest;
pub mod runtime;

use std::path::{Path, PathBuf};

use cln_layout::Layout;

pub use artifact::{detect, Artifact, DetectError};
pub use extract::{extract, ExtractError, Extracted};
pub use inspect::{inspect, Inspection};
pub use invoke::{exit_code, invoke, Invocation, InvokeError};
pub use manifest::{Entry, Kind, Manifest, ManifestError};
pub use runtime::{resolve_runtime, ResolvedRuntime, RuntimeError, RuntimeSource};

/// The archive-relative config a bundle carries (§00.13's dispatch table,
/// CLNH-10a). The framework generates it at package time.
pub const BUNDLE_CONFIG: &str = "config/host.toml";

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Detect(#[from] DetectError),

    #[error(transparent)]
    Extract(#[from] ExtractError),

    #[error(transparent)]
    Manifest(#[from] ManifestError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error(transparent)]
    Invoke(#[from] InvokeError),

    #[error("the bundle declares no {BUNDLE_CONFIG}, so there is no configuration to run it with")]
    BundleHasNoConfig,

    #[error("the bundle's manifest names '{wasm}', which is not in the archive")]
    MissingComponent { wasm: String },

    #[error("could not prepare the run cache: {source}")]
    Cache {
        #[source]
        source: std::io::Error,
    },

    #[error("running a project directory is not supported yet")]
    ProjectDirectory { path: PathBuf },

    #[error("{} is a bare component, not a package", .path.display())]
    NotAPackage { path: PathBuf },
}

impl RunError {
    /// The command that fixes this, rendered as the `help:` line.
    pub fn remedy(&self) -> Option<String> {
        match self {
            RunError::Detect(e) => e.remedy(),
            RunError::Extract(e) => e.remedy(),
            RunError::Manifest(e) => e.remedy(),
            RunError::Runtime(e) => e.remedy(),
            RunError::Invoke(e) => e.remedy(),
            RunError::BundleHasNoConfig | RunError::MissingComponent { .. } => {
                Some("the bundle looks incomplete; re-run `cln package` to rebuild it".into())
            }
            RunError::ProjectDirectory { path } => Some(format!(
                "run `cln build {}` first, then `cln run` the .clapp it produces",
                path.display()
            )),
            RunError::NotAPackage { .. } => Some(
                "a bare .wasm carries no manifest to inspect; run it with `cln run <file>.wasm`"
                    .into(),
            ),
            RunError::Cache { .. } => None,
        }
    }
}

/// What `run` decided, for `--verbose` reporting. Produced whether or not the
/// guest succeeded.
#[derive(Clone, Debug)]
pub struct Plan {
    pub runtime: ResolvedRuntime,
    pub world: String,
    pub wasm: PathBuf,
    pub config: PathBuf,
    /// Present for a bundle; `None` for a bare component.
    pub package: Option<String>,
}

/// Options a caller may vary. Everything here has a defensible default, so a
/// plain `cln run app.clapp` needs none of it.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Which world to run, for a `.serve` bundle declaring several. Ignored by
    /// a `.clapp`, which has one component whatever host runs it.
    pub world: Option<String>,
    /// An operator-supplied config, replacing whatever the artifact carries
    /// (§00.13's dispatch table, final row).
    pub config: Option<PathBuf>,
    /// Arguments forwarded to the guest after `--`.
    pub guest_args: Vec<String>,
    /// The project whose `.cln/runtime-version` applies, when the run happens
    /// inside one.
    pub project_root: Option<PathBuf>,
}

/// Plan a run without executing it.
///
/// Split from [`run`] so every decision is assertable in a test without a
/// runtime binary present, and so `--verbose` can report the plan before the
/// guest's own output starts.
pub fn plan(path: &Path, opts: &Options, layout: &Layout) -> Result<(Plan, Invocation), RunError> {
    match detect(path)? {
        Artifact::Bundle(bundle) => plan_bundle(&bundle, opts, layout),
        Artifact::Wasm(wasm) => plan_wasm(&wasm, opts, layout),
        Artifact::Project(dir) => Err(RunError::ProjectDirectory { path: dir }),
    }
}

fn plan_bundle(
    bundle: &Path,
    opts: &Options,
    layout: &Layout,
) -> Result<(Plan, Invocation), RunError> {
    let extracted = extract(bundle, &run_cache(layout))?;
    let entry = extracted.manifest.entry(opts.world.as_deref())?;

    let wasm = extracted.join(&entry.wasm);
    if !wasm.is_file() {
        return Err(RunError::MissingComponent {
            wasm: entry.wasm.clone(),
        });
    }

    // The operator's `--config` replaces the bundle's, unchanged (§00.13).
    let config = match &opts.config {
        Some(path) => path.clone(),
        None => {
            let carried = extracted.join(BUNDLE_CONFIG);
            if !carried.is_file() {
                return Err(RunError::BundleHasNoConfig);
            }
            carried
        }
    };

    let runtime = resolve_runtime(
        extracted.manifest.runtime_pin().as_ref(),
        opts.project_root.as_deref(),
        layout,
    )?;

    let invocation = Invocation {
        binary: runtime.binary.clone(),
        world: entry.world.clone(),
        wasm: wasm.clone(),
        config: config.clone(),
        assets: assets_dir(&extracted),
        guest_args: opts.guest_args.clone(),
        // The archive root, so a guest opening a relative path sees the
        // bundle's own layout rather than wherever the user happened to stand.
        cwd: Some(extracted.root.clone()),
    };

    let plan = Plan {
        runtime,
        world: entry.world,
        wasm,
        config,
        package: Some(format!(
            "{} {}",
            extracted.manifest.package.name, extracted.manifest.package.version
        )),
    };

    Ok((plan, invocation))
}

fn plan_wasm(wasm: &Path, opts: &Options, layout: &Layout) -> Result<(Plan, Invocation), RunError> {
    let world = opts
        .world
        .clone()
        .unwrap_or_else(|| devconfig::DEFAULT_WORLD.to_string());

    let wasm = wasm.canonicalize().unwrap_or_else(|_| wasm.to_path_buf());

    // A bare component carries no manifest, so there is nothing to pin against
    // and nothing to extract. The cache is used only to hold the generated
    // config, keyed by the component's own hash so two components never share
    // one — and so re-running the same component reuses its directory.
    let config = match &opts.config {
        Some(path) => path.clone(),
        None => {
            let sha = extract::file_sha256(&wasm)?;
            let dir = run_cache(layout).join(sha);
            devconfig::write(&dir, &devconfig::name_for(&wasm), &world, &wasm)
                .map_err(|source| RunError::Cache { source })?
        }
    };

    // No artifact pin: a bare component declares nothing about which runtime
    // built it, which is exactly the interop case §00.14 describes.
    let runtime = resolve_runtime(None, opts.project_root.as_deref(), layout)?;

    let invocation = Invocation {
        binary: runtime.binary.clone(),
        world: world.clone(),
        wasm: wasm.clone(),
        config: config.clone(),
        assets: None,
        // The user's own directory: a bare component is run in place, so
        // relative paths it opens should mean what the user meant by them.
        cwd: None,
        guest_args: opts.guest_args.clone(),
    };

    let plan = Plan {
        runtime,
        world,
        wasm,
        config,
        package: None,
    };

    Ok((plan, invocation))
}

/// Run the artifact and return the guest's exit code.
pub fn run(path: &Path, opts: &Options, layout: &Layout) -> Result<i32, RunError> {
    let (_, invocation) = plan(path, opts, layout)?;
    Ok(invoke(&invocation)?)
}

/// `~/.cln/cache/run/` — where bundles are unpacked (§00.13 step 3).
pub(crate) fn run_cache(layout: &Layout) -> PathBuf {
    layout.cache_dir().join("run")
}

/// A bundle's `assets/` directory, when it carries one.
///
/// Passed as `--assets` only if it exists: the flag names a directory the host
/// will read, and pointing it at a missing path would turn an optional part of
/// the format into a startup failure.
fn assets_dir(extracted: &Extracted) -> Option<PathBuf> {
    let dir = extracted.join("assets");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cln_shared::ToolchainKind;
    use semver::Version;
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

    fn manifest_toml(runtime_version: &str) -> String {
        format!(
            r#"
spec_version = "1"
[package]
name = "hello-world"
version = "0.1.0"
[build]
runtime_version = "{runtime_version}"
[artifact]
kind = "clapp"
worlds = ["cli"]
entry_wasm = "app.wasm"
"#
        )
    }

    /// A bundle shaped exactly like the one `clean-framework` produces.
    fn clapp(dir: &Path, runtime_version: &str) -> PathBuf {
        let bytes = zip_bytes(&[
            ("manifest.toml", manifest_toml(runtime_version).as_bytes()),
            ("app.wasm", b"\0asm\x0d\x00\x01\x00"),
            (
                "config/host.toml",
                b"[guest]\nwasm = \"../app.wasm\"\nworld = \"cli-default\"\n",
            ),
        ]);
        let p = dir.join("hello-world.clapp");
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn layout_with_runtime(versions: &[&str], active: Option<&str>) -> (tempfile::TempDir, Layout) {
        let home = tempdir().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();
        for s in versions {
            let v: Version = s.parse().unwrap();
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

    /// The end-to-end plan for the Phase 4 test target, minus the spawn.
    #[test]
    fn a_clapp_plans_the_specified_invocation() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "unknown");
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let (plan, inv) = plan(&bundle, &Options::default(), &layout).unwrap();

        assert_eq!(plan.world, "cli");
        assert_eq!(plan.package.as_deref(), Some("hello-world 0.1.0"));
        assert_eq!(plan.runtime.source, RuntimeSource::Active);

        let argv: Vec<String> = inv
            .argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "--world=cli");
        assert!(argv[1].ends_with("/app.wasm"));
        assert!(argv[2].ends_with("/config/host.toml"));
    }

    /// The load-bearing arrangement: the config the runtime is handed must be
    /// one directory below the component, so its `../app.wasm` resolves.
    #[test]
    fn the_config_sits_one_level_below_the_component() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "unknown");
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let (plan, _) = plan(&bundle, &Options::default(), &layout).unwrap();

        assert_eq!(plan.config.parent().unwrap().file_name().unwrap(), "config");
        assert_eq!(
            plan.config.parent().unwrap().parent().unwrap(),
            plan.wasm.parent().unwrap(),
            "config/ must be a child of the directory holding app.wasm"
        );
        // The path the host actually follows.
        assert!(plan.config.parent().unwrap().join("../app.wasm").exists());
    }

    #[test]
    fn the_run_cache_lives_under_the_layout() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "unknown");
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let (plan, _) = plan(&bundle, &Options::default(), &layout).unwrap();
        assert!(plan.wasm.starts_with(layout.cache_dir().join("run")));
    }

    /// A real semver pin binds strictly, even when a different runtime is
    /// active and would otherwise work.
    #[test]
    fn an_artifact_pin_selects_its_runtime_over_the_active_one() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "2.0.0");
        let (_h, layout) = layout_with_runtime(&["1.0.0", "2.0.0"], Some("1.0.0"));

        let (plan, _) = plan(&bundle, &Options::default(), &layout).unwrap();
        assert_eq!(plan.runtime.version, Version::new(2, 0, 0));
        assert_eq!(plan.runtime.source, RuntimeSource::ArtifactPin);
    }

    #[test]
    fn a_pinned_runtime_that_is_not_installed_names_the_install_command() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "9.9.9");
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let err = plan(&bundle, &Options::default(), &layout).unwrap_err();
        assert!(matches!(
            err,
            RunError::Runtime(RuntimeError::ArtifactPinMissing { .. })
        ));
        assert_eq!(err.remedy().unwrap(), "run `cln install runtime 9.9.9`");
    }

    /// The decision recorded in `Manifest::runtime_pin`: today's artifacts stamp
    /// `"unknown"` and must still run.
    #[test]
    fn an_unpinned_artifact_falls_through_to_the_active_runtime() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "unknown");
        let (_h, layout) = layout_with_runtime(&["1.4.2"], Some("1.4.2"));

        let (plan, _) = plan(&bundle, &Options::default(), &layout).unwrap();
        assert_eq!(plan.runtime.version, Version::new(1, 4, 2));
    }

    #[test]
    fn an_operator_config_replaces_the_bundles_own() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "unknown");
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let mine = tmp.path().join("mine.toml");
        std::fs::write(&mine, b"[host]\n").unwrap();

        let opts = Options {
            config: Some(mine.clone()),
            ..Default::default()
        };
        let (plan, _) = plan(&bundle, &opts, &layout).unwrap();
        assert_eq!(plan.config, mine);
    }

    #[test]
    fn guest_arguments_reach_the_invocation() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "unknown");
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let opts = Options {
            guest_args: vec!["--loud".into(), "file.txt".into()],
            ..Default::default()
        };
        let (_, inv) = plan(&bundle, &opts, &layout).unwrap();
        let argv: Vec<String> = inv
            .argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();

        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[sep + 1..], &["--loud", "file.txt"]);
    }

    /// A bundle whose config the framework failed to generate cannot run, and
    /// CLNH-13 makes that a startup error — so say it here, with the artifact
    /// named, rather than letting the host fail on a path.
    #[test]
    fn a_bundle_without_a_config_is_refused_before_spawning() {
        let tmp = tempdir().unwrap();
        let bytes = zip_bytes(&[
            ("manifest.toml", manifest_toml("unknown").as_bytes()),
            ("app.wasm", b"\0asm"),
        ]);
        let bundle = tmp.path().join("no-config.clapp");
        std::fs::write(&bundle, bytes).unwrap();
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let err = plan(&bundle, &Options::default(), &layout).unwrap_err();
        assert!(matches!(err, RunError::BundleHasNoConfig));
        assert!(err.remedy().unwrap().contains("cln package"));
    }

    #[test]
    fn a_manifest_naming_a_component_the_archive_lacks_is_refused() {
        let tmp = tempdir().unwrap();
        let bytes = zip_bytes(&[
            ("manifest.toml", manifest_toml("unknown").as_bytes()),
            ("config/host.toml", b"[guest]\n"),
        ]);
        let bundle = tmp.path().join("no-wasm.clapp");
        std::fs::write(&bundle, bytes).unwrap();
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let err = plan(&bundle, &Options::default(), &layout).unwrap_err();
        assert!(matches!(err, RunError::MissingComponent { .. }));
    }

    /// `assets/` is optional; passing `--assets` at a path that does not exist
    /// would turn an optional part of the format into a startup failure.
    #[test]
    fn assets_are_passed_only_when_the_bundle_carries_them() {
        let tmp = tempdir().unwrap();
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let plain = clapp(tmp.path(), "unknown");
        let (_, inv) = plan(&plain, &Options::default(), &layout).unwrap();
        assert!(inv.assets.is_none());

        let with_assets = zip_bytes(&[
            ("manifest.toml", manifest_toml("unknown").as_bytes()),
            ("app.wasm", b"\0asm"),
            ("config/host.toml", b"[guest]\n"),
            ("assets/icon.png", b"png"),
        ]);
        let p = tmp.path().join("with-assets.clapp");
        std::fs::write(&p, with_assets).unwrap();

        let (_, inv) = plan(&p, &Options::default(), &layout).unwrap();
        assert!(inv.assets.is_some(), "a carried assets/ must be passed");
    }

    /// A bare component has no config of its own, so manager writes the
    /// development-defaults one CLNH-13 requires.
    #[test]
    fn a_bare_wasm_gets_a_generated_development_config() {
        let tmp = tempdir().unwrap();
        let wasm = tmp.path().join("app.wasm");
        std::fs::write(&wasm, b"\0asm\x0d\x00\x01\x00").unwrap();
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let (plan, inv) = plan(&wasm, &Options::default(), &layout).unwrap();

        assert_eq!(
            plan.world, "cli",
            "cli is the default world for a bare component"
        );
        assert!(plan.package.is_none());
        assert!(plan.config.starts_with(layout.cache_dir().join("run")));

        let text = std::fs::read_to_string(&plan.config).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(v["host"]["deployment-mode"].as_str(), Some("development"));

        // Run in place: a bare component's relative paths are the user's.
        assert!(inv.cwd.is_none());
    }

    #[test]
    fn a_bare_wasm_honors_an_explicit_world() {
        let tmp = tempdir().unwrap();
        let wasm = tmp.path().join("svc.wasm");
        std::fs::write(&wasm, b"\0asm").unwrap();
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let opts = Options {
            world: Some("server".into()),
            ..Default::default()
        };
        let (plan, _) = plan(&wasm, &opts, &layout).unwrap();
        assert_eq!(plan.world, "server");
    }

    /// Deferred, not silently broken: the error names the two commands that do
    /// the same thing today.
    #[test]
    fn a_project_directory_is_refused_with_the_commands_that_work() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("clean.toml"), b"[project]\n").unwrap();
        let (_h, layout) = layout_with_runtime(&["1.0.0"], Some("1.0.0"));

        let err = plan(tmp.path(), &Options::default(), &layout).unwrap_err();
        assert!(matches!(err, RunError::ProjectDirectory { .. }));
        let remedy = err.remedy().unwrap();
        assert!(remedy.contains("cln build"), "{remedy}");
        assert!(remedy.contains("cln run"), "{remedy}");
    }

    #[test]
    fn a_missing_path_is_reported_before_any_runtime_lookup() {
        let tmp = tempdir().unwrap();
        // No runtime installed at all: detection must still fail first, since
        // the missing file is the more useful thing to say.
        let (_h, layout) = layout_with_runtime(&[], None);

        let err = plan(
            &tmp.path().join("ghost.clapp"),
            &Options::default(),
            &layout,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RunError::Detect(DetectError::NotFound { .. })
        ));
    }

    #[test]
    fn with_no_runtime_installed_the_error_names_the_install_command() {
        let tmp = tempdir().unwrap();
        let bundle = clapp(tmp.path(), "unknown");
        let (_h, layout) = layout_with_runtime(&[], None);

        let err = plan(&bundle, &Options::default(), &layout).unwrap_err();
        assert!(matches!(err, RunError::Runtime(RuntimeError::NoRuntime)));
        assert!(err.remedy().unwrap().contains("cln install runtime latest"));
    }
}
