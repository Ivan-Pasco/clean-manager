//! `cln register` / `cln unregister` end-to-end, through the real binary.
//!
//! These assert the properties a user would notice: that registering twice
//! does not accumulate, that unregistering leaves nothing behind, and that a
//! platform without an implementation says so instead of pretending.
//!
//! The macOS cases drive `~/Applications` through a redirected `HOME`, so a
//! test run never touches the developer's real applications directory or the
//! real Launch Services database.

use std::path::PathBuf;
use std::process::{Command, Output};

fn cln() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cln"))
}

/// Run `cln` with `CLN_HOME` and `HOME` redirected into scratch space.
fn run(cln_home: &std::path::Path, home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(cln())
        .args(args)
        .env("CLN_HOME", cln_home)
        .env("HOME", home)
        .output()
        .expect("cln should run")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A fake `cln` at `~/.cln/bin/cln` — the path registration binds to.
fn fake_shim(cln_home: &std::path::Path) {
    let bin = cln_home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("cln");
    std::fs::write(&shim, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A fresh machine reports nothing registered, and says how to change that.
#[test]
fn status_on_a_fresh_machine_reports_nothing_registered() {
    let cln_home = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    let out = run(cln_home.path(), home.path(), &["register", "--status"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let text = stdout(&out);
    #[cfg(target_os = "macos")]
    assert!(text.contains("no Clean file associations"), "{text}");
    #[cfg(not(target_os = "macos"))]
    assert!(text.contains("not implemented"), "{text}");
}

/// A local release directory holding one runtime, so `cln install` succeeds
/// offline. `CLN_RELEASES_DIR` points the installer at it.
///
/// Only the macOS cases install a toolchain — elsewhere registration is
/// unimplemented, so there is nothing to assert about what an install
/// registered. Gated to match, because `-D warnings` in CI makes a helper that
/// is dead on Linux a build failure rather than a lint.
///
/// The installer resolves a *packaged asset* named for the platform, plus its
/// `.sha256` sidecar — the same shape the release workflow publishes — so the
/// fixture builds a real tarball rather than dropping a loose binary.
#[cfg(target_os = "macos")]
fn local_release(dir: &std::path::Path) {
    let v = dir.join("runtime").join("9.9.9");
    std::fs::create_dir_all(&v).unwrap();

    // Stage the binary, then tar it under the name the platform matcher wants.
    let stage = dir.join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let bin = stage.join("clean-runtime");
    std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let arch = if arch == "aarch64" { "arm64" } else { arch };
    let asset = v.join(format!("clean-runtime-9.9.9-{os}-{arch}.tar.gz"));

    let status = Command::new("tar")
        .arg("czf")
        .arg(&asset)
        .arg("-C")
        .arg(&stage)
        .arg("clean-runtime")
        .status()
        .expect("tar should run");
    assert!(status.success(), "failed to build the release fixture");

    let bytes = std::fs::read(&asset).unwrap();
    std::fs::write(
        asset.with_file_name(format!(
            "{}.sha256",
            asset.file_name().unwrap().to_string_lossy()
        )),
        format!("{}  {}\n", sha256_hex(&bytes), asset.display()),
    )
    .unwrap();
}

/// SHA-256 of the fixture archive, so the installer's integrity check passes.
#[cfg(target_os = "macos")]
fn sha256_hex(bytes: &[u8]) -> String {
    use std::io::Write as _;
    let mut child = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("shasum should run");
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// Install a runtime from a local release dir, with the opt-out controls the
/// caller wants applied.
#[cfg(target_os = "macos")]
fn install_runtime(
    cln_home: &std::path::Path,
    home: &std::path::Path,
    extra_args: &[&str],
    no_register_env: Option<&str>,
) -> Output {
    // Held for the whole call: dropping it would delete the release tree out
    // from under the child process.
    let releases = tempfile::tempdir().unwrap();
    local_release(releases.path());

    let mut args = vec!["install", "runtime", "9.9.9"];
    args.extend_from_slice(extra_args);

    let mut cmd = Command::new(cln());
    cmd.args(&args)
        .env("CLN_HOME", cln_home)
        .env("HOME", home)
        .env("CLN_RELEASES_DIR", releases.path());
    if let Some(v) = no_register_env {
        cmd.env("CLN_NO_REGISTER", v);
    }
    cmd.output().expect("cln install should run")
}

/// Unregistering when nothing is registered must succeed: `cln uninstall`
/// calls it unconditionally, on a machine that may never have opted in.
#[test]
fn unregister_is_safe_when_nothing_is_registered() {
    let cln_home = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();

    let out = run(cln_home.path(), home.path(), &["unregister"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("nothing was registered"));
}

/// On a platform with no implementation, registering must fail loudly and name
/// a command that does work — never report a success that did nothing.
#[cfg(not(target_os = "macos"))]
#[test]
fn registering_on_an_unsupported_platform_fails_clearly() {
    let cln_home = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fake_shim(cln_home.path());

    let out = run(cln_home.path(), home.path(), &["register"]);
    assert!(
        !out.status.success(),
        "registration must not report success on a platform it cannot support"
    );

    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("not implemented"), "{text}");
    assert!(text.contains("cln run"), "{text}");
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    fn bundle(home: &std::path::Path) -> PathBuf {
        home.join("Applications").join("Clean.app")
    }

    /// §00.12's default: installing registers, with no extra command.
    #[test]
    fn installing_registers_automatically() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        let out = install_runtime(cln_home.path(), home.path(), &[], None);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            bundle(home.path()).exists(),
            "install must register: {}",
            stdout(&out)
        );
    }

    /// `--no-register` declines without disabling anything else.
    #[test]
    fn install_no_register_skips_registration() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        let out = install_runtime(cln_home.path(), home.path(), &["--no-register"], None);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            !bundle(home.path()).exists(),
            "--no-register must not register"
        );
        // The install itself still happened.
        assert!(stdout(&out).contains("runtime"), "{}", stdout(&out));
    }

    /// The environment opt-out, for scripted installs that cannot add a flag.
    #[test]
    fn cln_no_register_env_skips_registration() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        let out = install_runtime(cln_home.path(), home.path(), &[], Some("1"));
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            !bundle(home.path()).exists(),
            "CLN_NO_REGISTER must be honored"
        );
    }

    /// `CLN_NO_REGISTER=0` is not an opt-out — it reads as "no, don't skip".
    #[test]
    fn cln_no_register_zero_still_registers() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        let out = install_runtime(cln_home.path(), home.path(), &[], Some("0"));
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(bundle(home.path()).exists(), "0 must not read as opt-out");
    }

    /// The load-bearing half of the opt-out: §00.12 forbids a later install
    /// from silently undoing an explicit `cln unregister`.
    #[test]
    fn an_explicit_unregister_survives_a_later_install() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        assert!(install_runtime(cln_home.path(), home.path(), &[], None)
            .status
            .success());
        assert!(bundle(home.path()).exists());

        // The user declines.
        assert!(run(cln_home.path(), home.path(), &["unregister"])
            .status
            .success());
        assert!(!bundle(home.path()).exists());

        // A later install must respect that.
        let out = install_runtime(cln_home.path(), home.path(), &[], None);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            !bundle(home.path()).exists(),
            "an install must not undo an explicit unregister: {}",
            stdout(&out)
        );
    }

    /// Asking for it back must work without needing to know about a flag.
    #[test]
    fn register_after_unregister_opts_back_in() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_shim(cln_home.path());

        assert!(run(cln_home.path(), home.path(), &["register"])
            .status
            .success());
        assert!(run(cln_home.path(), home.path(), &["unregister"])
            .status
            .success());

        // Explicit re-register clears the decline...
        assert!(run(cln_home.path(), home.path(), &["register"])
            .status
            .success());
        assert!(bundle(home.path()).exists());

        // ...and a later install keeps registering.
        std::fs::remove_dir_all(bundle(home.path())).unwrap();
        assert!(install_runtime(cln_home.path(), home.path(), &[], None)
            .status
            .success());
        assert!(bundle(home.path()).exists());
    }

    /// The core promise: after registering, an app bundle exists that Finder
    /// can bind `.clapp` to, and it invokes the recorded binary.
    #[test]
    fn registering_creates_a_bundle_bound_to_the_shim() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_shim(cln_home.path());

        let out = run(cln_home.path(), home.path(), &["register"]);
        assert!(out.status.success(), "{}", stderr(&out));

        let b = bundle(home.path());
        assert!(b.join("Contents/Info.plist").is_file());
        assert!(b.join("Contents/MacOS/cln-open").is_file());

        // It must invoke the stable shim, not a transient build path.
        let launcher = std::fs::read_to_string(b.join("Contents/MacOS/cln-open")).unwrap();
        assert!(
            launcher.contains(&cln_home.path().join("bin/cln").display().to_string()),
            "the launcher must call the ~/.cln/bin/cln shim"
        );

        // And the state file records it, so unregister knows what to remove.
        let state =
            std::fs::read_to_string(cln_home.path().join("registrations/state.toml")).unwrap();
        assert!(state.contains("[clapp]"), "{state}");
        assert!(state.contains("[serve]"), "{state}");
    }

    /// Reinstalling or upgrading must not duplicate or corrupt the
    /// registration — the idempotence requirement.
    #[test]
    fn registering_twice_leaves_exactly_one_bundle() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_shim(cln_home.path());

        assert!(run(cln_home.path(), home.path(), &["register"])
            .status
            .success());
        let first =
            std::fs::read_to_string(bundle(home.path()).join("Contents/Info.plist")).unwrap();

        assert!(run(cln_home.path(), home.path(), &["register"])
            .status
            .success());
        let second =
            std::fs::read_to_string(bundle(home.path()).join("Contents/Info.plist")).unwrap();

        assert_eq!(first, second, "re-registering must converge");

        let apps: Vec<_> = std::fs::read_dir(home.path().join("Applications"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(apps.len(), 1, "expected exactly one bundle, got {apps:?}");
    }

    /// An association must not outlive the toolchain: unregister removes the
    /// bundle so no `.clapp` is left pointing at a binary that may go away.
    #[test]
    fn unregistering_removes_the_bundle_and_the_state() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_shim(cln_home.path());

        assert!(run(cln_home.path(), home.path(), &["register"])
            .status
            .success());
        assert!(bundle(home.path()).exists());

        let out = run(cln_home.path(), home.path(), &["unregister"]);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            !bundle(home.path()).exists(),
            "the bundle must be gone after unregister"
        );

        let status = run(cln_home.path(), home.path(), &["register", "--status"]);
        assert!(stdout(&status).contains("no Clean file associations"));
    }

    /// §00.12: `.wasm` MUST NOT be claimed under any circumstance.
    #[test]
    fn the_registration_never_claims_wasm() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_shim(cln_home.path());

        assert!(run(cln_home.path(), home.path(), &["register"])
            .status
            .success());

        let plist =
            std::fs::read_to_string(bundle(home.path()).join("Contents/Info.plist")).unwrap();
        assert!(
            !plist.contains("wasm"),
            "the bundle must never claim .wasm: {plist}"
        );
    }

    /// `--status` must notice a bundle deleted behind manager's back rather
    /// than continuing to report a registration that no longer exists.
    #[test]
    fn status_reports_drift_when_the_bundle_is_deleted() {
        let cln_home = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_shim(cln_home.path());

        assert!(run(cln_home.path(), home.path(), &["register"])
            .status
            .success());
        std::fs::remove_dir_all(bundle(home.path())).unwrap();

        let out = run(cln_home.path(), home.path(), &["register", "--status"]);
        assert!(out.status.success());
        let err = stderr(&out);
        assert!(err.contains("no longer exists"), "{err}");
        assert!(
            err.contains("cln register"),
            "expected a repair hint: {err}"
        );
    }
}
