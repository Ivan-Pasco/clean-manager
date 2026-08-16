//! `cln register` / `cln unregister` — OS file associations (Manager §00.12).
//!
//! Registers `.clapp` and `.serve` with the host OS so a double-click
//! dispatches to `cln run <path>`. Per-user, no elevated privileges.
//!
//! # Scope: macOS is implemented; Windows and Linux are not
//!
//! §00.12 specifies all three platforms and PLAN.md schedules the full set for
//! M3, behind a per-OS test matrix that does not exist yet. This crate
//! implements macOS properly and makes the other two [fail loudly][unsupported]
//! rather than half-work. A stub that silently did nothing would report success
//! and leave a user double-clicking a file that never opens.
//!
//! # `.wasm` is never claimed
//!
//! §00.12 forbids it under any circumstance: `.wasm` belongs to the wider
//! WebAssembly ecosystem, and claiming it would break every Rust, Go, and
//! AssemblyScript workflow on the machine. [`Extension`] is a closed enum with
//! no `Wasm` variant, so this is enforced by the type system rather than by
//! remembering.
//!
//! # Idempotence and removal
//!
//! [`register`] fully regenerates the OS-side artifacts, so running it twice
//! converges instead of accumulating, and an upgrade rebinds the association to
//! the new binary path. [`unregister`] removes them, so an association never
//! outlives the toolchain it points at.
//!
//! # Registration is automatic, with an opt-out
//!
//! §00.12 has `cln install` register at the end of a successful install, so a
//! double-click works without a separate command. The user declines with
//! `cln install --no-register`, `CLN_NO_REGISTER=1`, or by running
//! `cln unregister` — which is remembered via [`Reason::UserRequested`], so a
//! later install does not silently undo the choice.

pub mod state;
pub mod unsupported;

#[cfg(target_os = "macos")]
pub mod macos;

use std::path::{Path, PathBuf};

use cln_layout::Layout;

pub use state::{Extension, Record, State, StateError};

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("{0}")]
    Unsupported(String),

    #[error(transparent)]
    State(#[from] StateError),

    #[error("could not write the application bundle: {source}")]
    Bundle {
        #[source]
        source: std::io::Error,
    },

    #[error("could not tell Launch Services about the bundle: {0}")]
    LaunchServices(String),

    #[error("could not locate the cln binary to bind to: {0}")]
    NoBinary(String),
}

impl RegisterError {
    /// The `help:` line, where one exists.
    pub fn remedy(&self) -> Option<String> {
        match self {
            RegisterError::Unsupported(_) => None,
            RegisterError::NoBinary(_) => {
                Some("reinstall the toolchain so `cln` sits at a stable path".into())
            }
            RegisterError::LaunchServices(_) => Some(
                "run `cln register` again, or open a .clapp once via Finder's Open With".into(),
            ),
            _ => None,
        }
    }
}

/// What a register or unregister call did, for reporting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    /// Which extensions are now bound.
    pub extensions: Vec<Extension>,
    /// The OS-side artifact created or removed.
    pub os_path: Option<PathBuf>,
    /// The binary the association invokes.
    pub bound_binary: Option<PathBuf>,
    /// True when the registration already matched and nothing changed.
    pub unchanged: bool,
}

/// Whether this platform supports registration at all.
pub const fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// Register every Clean extension with the OS.
///
/// `cln` is the binary the association will invoke; `version` is stamped into
/// the bundle; `now` is an RFC 3339 timestamp supplied by the caller (this
/// crate does not read the clock, which keeps the state file deterministic
/// under test).
pub fn register(
    layout: &Layout,
    cln: &Path,
    version: &str,
    now: &str,
) -> Result<Outcome, RegisterError> {
    if !cln.is_file() {
        return Err(RegisterError::NoBinary(format!(
            "{} does not exist",
            cln.display()
        )));
    }
    register_impl(layout, cln, version, now)
}

/// Why a registration is being removed.
///
/// The distinction matters because §00.12 requires an explicit `cln
/// unregister` to survive later installs, while manager's own housekeeping
/// must not: if removing the last runtime were recorded as the user declining,
/// reinstalling a runtime would leave double-click silently off with nothing
/// to explain it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    /// The user ran `cln unregister`. Remembered; blocks auto-registration.
    UserRequested,
    /// Manager withdrew an association it could no longer honor. Forgotten;
    /// a later install registers again as normal.
    Housekeeping,
}

/// Remove every Clean registration.
///
/// Safe to call when nothing is registered — that is the normal case during an
/// uninstall on a machine that never opted in, and it must not fail there.
pub fn unregister(layout: &Layout, reason: Reason) -> Result<Outcome, RegisterError> {
    unregister_impl(layout, reason)
}

/// Report what is registered, and whether the OS still agrees.
///
/// Returns the recorded state plus any drift detected: a bundle that has been
/// deleted, or one bound to a `cln` that no longer exists. §00.12 requires
/// `--status` to notice when another tool has taken the association over.
pub fn status(layout: &Layout) -> Result<Status, RegisterError> {
    let recorded = state::load(layout)?;
    let mut drift = Vec::new();

    for ext in Extension::ALL {
        let Some(record) = recorded.get(ext) else {
            continue;
        };
        if !record.registered {
            continue;
        }
        if let Some(p) = &record.os_path {
            if !p.exists() {
                drift.push(format!(
                    "{ext} is recorded as registered, but {} no longer exists",
                    p.display()
                ));
            }
        }
        if let Some(b) = &record.bound_binary {
            if !b.exists() {
                drift.push(format!(
                    "{ext} points at {}, which no longer exists",
                    b.display()
                ));
            }
        }
    }

    Ok(Status {
        state: recorded,
        drift,
        supported: supported(),
    })
}

#[derive(Clone, Debug)]
pub struct Status {
    pub state: State,
    /// Human-readable descriptions of registrations that no longer hold.
    pub drift: Vec<String>,
    pub supported: bool,
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn register_impl(
    layout: &Layout,
    cln: &Path,
    version: &str,
    now: &str,
) -> Result<Outcome, RegisterError> {
    let home = home_dir()?;
    let bundle = macos::bundle_path(&home);

    // Withdraw the previous registration *before* the bundle is rewritten.
    //
    // `lsregister -f` adds a claim; it does not replace the one already there.
    // Re-registering therefore accumulates duplicate claims on the same UTI,
    // and once several exist the type goes `inactive` and Finder falls back to
    // the generic icon for whatever the type conforms to — a `.clapp` starts
    // showing the system ZIP icon, because the UTI conforms to `public.archive`.
    // The association still works, so nothing fails; only the icon is wrong,
    // which makes this easy to misread as an icon bug.
    //
    // Ignored on failure: there may be nothing registered yet, which is the
    // normal first-install case.
    if bundle.exists() {
        let _ = unregister_with_launch_services(&bundle);
    }

    let bundle = macos::write_bundle(&home, cln, version)
        .map_err(|source| RegisterError::Bundle { source })?;

    // Launch Services caches aggressively; without this the bundle exists but
    // Finder keeps opening .clapp with whatever it decided before.
    register_with_launch_services(&bundle)?;

    let mut st = state::load(layout)?;
    for ext in Extension::ALL {
        st.set(
            ext,
            Record {
                registered: true,
                // Asking to register is unambiguous: it clears an earlier
                // decline, so `cln register` after `cln unregister` works
                // without the user having to know about the flag.
                declined: false,
                os_path: Some(bundle.clone()),
                bound_binary: Some(cln.to_path_buf()),
                registered_at: Some(now.to_string()),
            },
        );
    }
    state::save(layout, &st)?;

    Ok(Outcome {
        extensions: Extension::ALL.to_vec(),
        os_path: Some(bundle),
        bound_binary: Some(cln.to_path_buf()),
        unchanged: false,
    })
}

#[cfg(target_os = "macos")]
fn unregister_impl(layout: &Layout, reason: Reason) -> Result<Outcome, RegisterError> {
    let mut st = state::load(layout)?;

    // Only ever remove a bundle this `~/.cln/` recorded creating.
    //
    // The previous version fell back to `$HOME/Applications/Clean.app` when the
    // state file named no path, which made "unregister with an empty state"
    // mean "delete whatever bundle happens to be at the default location". A
    // test with a tempdir `CLN_HOME` — an empty state — therefore deleted the
    // developer's real registration on every run, and the symptom appeared much
    // later as Finder offering to search the App Store for a handler.
    //
    // Nothing recorded means nothing of ours to remove. That is also the honest
    // reading: manager should not delete an application it cannot show it
    // installed.
    let recorded = st.get(Extension::Clapp).and_then(|r| r.os_path.clone());

    let removed = match recorded.as_deref() {
        Some(bundle) if bundle.exists() => {
            // Unregister with Launch Services *before* removing the bundle: it
            // reads the bundle to learn which types to release.
            let _ = unregister_with_launch_services(bundle);
            std::fs::remove_dir_all(bundle).map_err(|source| RegisterError::Bundle { source })?;
            Some(bundle.to_path_buf())
        }
        _ => None,
    };
    let existed = removed.is_some();

    // A user-requested removal is remembered so a later `cln install` honors
    // it (§00.12); housekeeping clears the entry instead, since a cleared
    // record reads as "never registered" and auto-registers again.
    for ext in Extension::ALL {
        match reason {
            Reason::UserRequested => st.set(ext, Record::declined_record()),
            Reason::Housekeeping => st.remove(ext),
        }
    }
    state::save(layout, &st)?;

    Ok(Outcome {
        extensions: Extension::ALL.to_vec(),
        os_path: removed,
        bound_binary: None,
        unchanged: !existed,
    })
}

/// Skip the Launch Services calls, for tests.
///
/// **Launch Services is machine-wide and ignores `HOME`.** A test can redirect
/// `HOME` so the bundle is written into a tempdir, but `lsregister` still
/// registers that tempdir path in the user's real database — and the tempdir is
/// deleted seconds later, leaving an entry pointing at nothing. Enough of those
/// and the OS resolves `.clapp` to a deleted bundle, which breaks double-click
/// on the developer's own machine. That is exactly what happened while building
/// this feature, so it is guarded rather than left to discipline.
///
/// Everything else still runs: the bundle is written, the state file is
/// updated, and every assertion about layout, idempotence, and removal holds.
/// Only the two `lsregister` calls are skipped.
#[cfg(target_os = "macos")]
fn skip_launch_services() -> bool {
    std::env::var_os("CLN_REGISTER_SKIP_LSREGISTER").is_some()
}

/// Tell Launch Services the bundle exists.
///
/// `lsregister` is not on `PATH`; it lives inside the CoreServices framework.
/// A failure here is reported rather than swallowed, because the bundle
/// existing without Launch Services knowing about it is exactly the silent
/// half-registration this crate is meant to avoid.
#[cfg(target_os = "macos")]
fn register_with_launch_services(bundle: &Path) -> Result<(), RegisterError> {
    if skip_launch_services() {
        return Ok(());
    }
    let out = std::process::Command::new(LSREGISTER)
        .arg("-f")
        .arg(bundle)
        .output()
        .map_err(|e| RegisterError::LaunchServices(format!("could not run lsregister: {e}")))?;

    if !out.status.success() {
        return Err(RegisterError::LaunchServices(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn unregister_with_launch_services(bundle: &Path) -> Result<(), RegisterError> {
    if skip_launch_services() {
        return Ok(());
    }
    let out = std::process::Command::new(LSREGISTER)
        .arg("-u")
        .arg(bundle)
        .output()
        .map_err(|e| RegisterError::LaunchServices(format!("could not run lsregister: {e}")))?;
    if !out.status.success() {
        return Err(RegisterError::LaunchServices(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

#[cfg(target_os = "macos")]
fn home_dir() -> Result<PathBuf, RegisterError> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| RegisterError::NoBinary("HOME is not set".into()))
}

// ---------------------------------------------------------------------------
// Windows / Linux — not implemented, and loud about it
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "macos"))]
fn register_impl(
    _layout: &Layout,
    _cln: &Path,
    _version: &str,
    _now: &str,
) -> Result<Outcome, RegisterError> {
    Err(RegisterError::Unsupported(unsupported::message()))
}

/// Unregistering on a platform that cannot register is *not* an error.
///
/// `cln uninstall` calls this unconditionally so an association never outlives
/// the toolchain. On a platform that never registered anything there is nothing
/// to undo, and failing here would make uninstall fail for no reason.
#[cfg(not(target_os = "macos"))]
fn unregister_impl(layout: &Layout, reason: Reason) -> Result<Outcome, RegisterError> {
    let mut st = state::load(layout)?;
    for ext in Extension::ALL {
        match reason {
            Reason::UserRequested => st.set(ext, Record::declined_record()),
            Reason::Housekeeping => st.remove(ext),
        }
    }
    state::save(layout, &st)?;
    Ok(Outcome {
        extensions: Vec::new(),
        os_path: None,
        bound_binary: None,
        unchanged: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn layout() -> (tempfile::TempDir, Layout) {
        let home = tempdir().unwrap();
        let l = Layout::new(home.path());
        l.ensure_base().unwrap();
        (home, l)
    }

    /// A machine that never registered must report cleanly rather than error.
    #[test]
    fn status_on_a_fresh_machine_is_empty_and_clean() {
        let (_h, l) = layout();
        let s = status(&l).unwrap();
        assert!(s.state.entries.is_empty());
        assert!(s.drift.is_empty());
    }

    #[test]
    fn registering_against_a_missing_binary_is_refused() {
        let (_h, l) = layout();
        let err =
            register(&l, Path::new("/nope/cln"), "0.1.9", "2026-08-16T00:00:00Z").unwrap_err();
        assert!(matches!(err, RegisterError::NoBinary(_)));
    }

    /// Uninstall calls this unconditionally; it must be safe when nothing was
    /// ever registered, on every platform.
    ///
    /// Asserts only on the state file, not on `unchanged`. With no recorded
    /// `os_path`, the macOS path falls back to `$HOME/Applications/Clean.app`
    /// to find a bundle an older manager may have left — so on a developer's
    /// own machine `unchanged` depends on whether *they* have Clean
    /// registered, which is not this test's subject and would make it pass or
    /// fail depending on the machine.
    #[test]
    fn unregistering_when_nothing_is_registered_succeeds() {
        let (_h, l) = layout();
        let out = unregister(&l, Reason::UserRequested).unwrap();
        assert!(
            out.unchanged,
            "nothing was recorded, so nothing was removed"
        );
        assert!(out.os_path.is_none());
        assert!(!state::load(&l).unwrap().is_registered(Extension::Clapp));
    }

    /// Unregistering an empty state must not touch a bundle it never recorded.
    ///
    /// This is the bug that broke the developer's own machine: `unregister`
    /// used to fall back to `$HOME/Applications/Clean.app`, so this very test —
    /// running with a tempdir `CLN_HOME`, and therefore an empty state —
    /// deleted the real registration on every run. The symptom surfaced much
    /// later, as Finder offering to search the App Store for a handler.
    #[test]
    fn unregistering_an_empty_state_deletes_nothing() {
        let (_h, l) = layout();

        // A bundle at the default location, which this layout never recorded.
        let home = tempdir().unwrap();
        let bystander = home.path().join("Applications").join("Clean.app");
        std::fs::create_dir_all(&bystander).unwrap();
        std::fs::write(bystander.join("marker"), b"not ours").unwrap();

        unregister(&l, Reason::UserRequested).unwrap();

        assert!(
            bystander.join("marker").exists(),
            "unregister must only remove a bundle this ~/.cln recorded"
        );
    }

    /// A registration whose bundle was deleted behind manager's back must be
    /// reported, not silently trusted (§00.12's drift requirement).
    #[test]
    fn status_reports_a_bundle_that_has_vanished() {
        let (_h, l) = layout();
        let mut st = State::default();
        st.set(
            Extension::Clapp,
            Record {
                registered: true,
                declined: false,
                os_path: Some(PathBuf::from("/nonexistent/Clean.app")),
                bound_binary: Some(PathBuf::from("/nonexistent/cln")),
                registered_at: Some("2026-08-16T00:00:00Z".into()),
            },
        );
        state::save(&l, &st).unwrap();

        let s = status(&l).unwrap();
        assert_eq!(s.drift.len(), 2, "both the bundle and binary are missing");
        assert!(s.drift.iter().any(|d| d.contains("Clean.app")));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn registering_on_an_unsupported_platform_fails_loudly() {
        let (_h, l) = layout();
        let tmp = tempdir().unwrap();
        let fake = tmp.path().join("cln");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();

        let err = register(&l, &fake, "0.1.9", "2026-08-16T00:00:00Z").unwrap_err();
        assert!(matches!(err, RegisterError::Unsupported(_)));
        let msg = err.to_string();
        assert!(msg.contains("not implemented"));
        assert!(msg.contains("cln run"));
    }

    #[cfg(target_os = "macos")]
    mod macos_tests {
        use super::*;

        /// Registering twice must converge, not accumulate — the property that
        /// makes reinstall and upgrade safe.
        #[test]
        fn registering_twice_is_idempotent() {
            let (_h, _l) = layout();
            let tmp = tempdir().unwrap();
            let fake_cln = tmp.path().join("cln");
            std::fs::write(&fake_cln, b"#!/bin/sh\n").unwrap();

            // Point HOME at a scratch dir so the test never writes to the real
            // ~/Applications.
            let fake_home = tempdir().unwrap();
            let bundle = macos::write_bundle(fake_home.path(), &fake_cln, "0.1.9").unwrap();
            let first = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();

            let bundle2 = macos::write_bundle(fake_home.path(), &fake_cln, "0.1.9").unwrap();
            let second = std::fs::read_to_string(bundle2.join("Contents/Info.plist")).unwrap();

            assert_eq!(bundle, bundle2);
            assert_eq!(first, second);

            // And exactly one bundle exists.
            let apps = fake_home.path().join("Applications");
            let count = std::fs::read_dir(&apps).unwrap().count();
            assert_eq!(count, 1, "re-registering must not create a second bundle");
        }
    }
}
