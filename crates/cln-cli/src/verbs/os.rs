//! `cln register`, `cln unregister` — OS file associations (Manager §00.12).

use anyhow::{Context, Result};
use clap::Args;
use cln_register::{self as reg, Extension};

use crate::env::Env;

/// The `cln` binary the association should invoke.
///
/// `~/.cln/bin/cln` rather than `std::env::current_exe()`: the shim in `bin/`
/// is the stable path across upgrades, so an association bound to it keeps
/// working when the underlying binary is replaced. Falling back to the running
/// executable covers a manager invoked from a build directory, where the shim
/// may not exist yet.
fn binary_to_bind(env: &Env) -> Result<std::path::PathBuf> {
    let shim = env.layout.bin_dir().join("cln");
    if shim.is_file() {
        return Ok(shim);
    }
    std::env::current_exe().context("locating the running cln binary")
}

/// An RFC 3339 timestamp for the state file.
///
/// Formatted here rather than in `cln-register` so that crate stays clock-free
/// and its state writes stay deterministic under test.
fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Civil-time conversion from a Unix timestamp (days since epoch → y/m/d),
    // so the state file carries a real date without pulling in a date crate
    // for one string.
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[derive(Args, Debug)]
pub struct RegisterArgs {
    /// Report what is registered instead of changing anything.
    #[arg(long)]
    pub status: bool,
}

pub fn register(args: RegisterArgs, env: &Env) -> Result<()> {
    if args.status {
        return status(env);
    }

    let cln = binary_to_bind(env)?;
    let outcome = reg::register(&env.layout, &cln, env!("CARGO_PKG_VERSION"), &now_rfc3339())
        .map_err(to_anyhow)?;

    let exts = outcome
        .extensions
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!("registered {exts} -> {}", cln.display());
    if let Some(p) = &outcome.os_path {
        println!("  via {}", p.display());
    }
    println!("double-click a .clapp to run it");
    Ok(())
}

#[derive(Args, Debug)]
pub struct UnregisterArgs {}

pub fn unregister(_args: UnregisterArgs, env: &Env) -> Result<()> {
    let outcome = reg::unregister(&env.layout).map_err(to_anyhow)?;
    if outcome.unchanged {
        println!("nothing was registered");
    } else {
        println!("removed Clean file associations");
    }
    Ok(())
}

fn status(env: &Env) -> Result<()> {
    let s = reg::status(&env.layout).map_err(to_anyhow)?;

    if !s.supported {
        println!("{}", cln_register::unsupported::message());
        return Ok(());
    }

    let any = Extension::ALL.iter().any(|e| s.state.is_registered(*e));
    if !any {
        println!("no Clean file associations are registered");
        println!("run `cln register` to enable double-click");
        return Ok(());
    }

    for ext in Extension::ALL {
        match s.state.get(ext) {
            Some(r) if r.registered => {
                println!("{ext}: registered");
                if let Some(b) = &r.bound_binary {
                    println!("  bound to {}", b.display());
                }
                if let Some(p) = &r.os_path {
                    println!("  via {}", p.display());
                }
            }
            _ => println!("{ext}: not registered"),
        }
    }

    for d in &s.drift {
        eprintln!("warning: {d}");
    }
    if !s.drift.is_empty() {
        eprintln!("help: run `cln register` to repair");
    }
    Ok(())
}

/// Carry the library's `help:` line through to the CLI's error rendering.
fn to_anyhow(e: reg::RegisterError) -> anyhow::Error {
    match e.remedy() {
        Some(r) => anyhow::anyhow!("{e}\n\nhelp: {r}"),
        None => anyhow::anyhow!("{e}"),
    }
}

/// Register as part of `cln install`, without failing the install.
///
/// Called by the install verb so a user who installs the toolchain can
/// double-click a `.clapp` immediately — the association is not a separate
/// manual step (§00.12 diverges here; see PLAN.md).
///
/// **Registration failure never fails an install.** The toolchain is fully
/// usable from a terminal without an association, so a Launch Services hiccup,
/// a read-only `~/Applications`, or an unsupported platform must not leave a
/// working install reported as failed. Anything that goes wrong is reported as
/// a warning naming `cln register`, so it is visible and retryable rather than
/// silent.
pub fn register_after_install(env: &Env) {
    if !reg::supported() {
        // Not a warning: this is the expected state on Windows and Linux
        // today, and an install there is not degraded by it.
        return;
    }

    let Ok(cln) = binary_to_bind(env) else {
        return;
    };
    if !cln.is_file() {
        return;
    }

    match reg::register(&env.layout, &cln, env!("CARGO_PKG_VERSION"), &now_rfc3339()) {
        Ok(_) => println!("registered .clapp, .serve -> double-click to run"),
        Err(e) => {
            eprintln!("warning: could not register file associations: {e}");
            eprintln!(
                "help: the toolchain works; run `cln register` to retry double-click support"
            );
        }
    }
}

/// Deregister after the last runtime is removed.
///
/// §00.12 couples registration state to binary lifetime: "a registered
/// extension pointing at a nonexistent binary is a broken user experience".
/// With no runtime installed, a double-click can only open a window that
/// reports it cannot resolve one — so the association is withdrawn instead,
/// and Finder falls back to its normal "no application set" behavior.
///
/// Like registration, this never fails the command that triggered it: the
/// uninstall itself succeeded, and a stale association is a smaller problem
/// than an uninstall that reports failure.
pub fn unregister_after_last_runtime(env: &Env) {
    if !reg::supported() {
        return;
    }
    match reg::unregister(&env.layout) {
        Ok(o) if !o.unchanged => {
            println!("removed file associations (no runtime left to run artifacts)");
        }
        Ok(_) => {}
        Err(e) => eprintln!("warning: could not remove file associations: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state file records a real date, so `--status` and any later audit
    /// can tell when a registration was made.
    #[test]
    fn the_timestamp_is_rfc3339_shaped() {
        let t = now_rfc3339();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z'));
        let (date, time) = t[..t.len() - 1].split_once('T').unwrap();
        assert_eq!(date.split('-').count(), 3);
        assert_eq!(time.split(':').count(), 3);

        let year: i32 = date[..4].parse().unwrap();
        assert!((2024..2100).contains(&year), "implausible year in {t}");
    }
}
