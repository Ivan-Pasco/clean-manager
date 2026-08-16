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
