//! `cln inspect <path>` and `cln open <path>` — Manager §00.12 (P-2, P-3).
//!
//! `inspect` prints what a package is. `open` is what a double-click reaches:
//! it shows the same facts in a window and offers the actions valid for the
//! package's kind, instead of executing straight away.
//!
//! **Opening does not run anything by itself.** A package may have arrived by
//! email or download, so the person opening it is shown what it is — including
//! whether it carries a signature — before any code executes (P-2). Only a
//! button press runs or deploys.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Result};
use clap::Args;
use cln_run::{inspect as do_inspect, Inspection};

use crate::env::Env;

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// The package to inspect.
    pub path: PathBuf,
}

pub fn inspect(args: InspectArgs, env: &Env) -> Result<ExitCode> {
    let i = do_inspect(&args.path, &env.layout).map_err(|e| match e.remedy() {
        Some(r) => anyhow!("{e}\n\nhelp: {r}"),
        None => anyhow!("{e}"),
    })?;

    println!("{} {}", i.name, i.version);
    if let Some(d) = &i.description {
        println!("{d}");
    }
    println!();
    println!("Type        {}", i.kind_label());
    if !i.worlds.is_empty() {
        println!("Worlds      {}", i.worlds.join(", "));
    }
    println!("Runtime     {}", runtime_line(&i));
    println!(
        "Signature   {}",
        if i.signed { "present" } else { "unsigned" }
    );

    Ok(ExitCode::SUCCESS)
}

/// The runtime line, which has to say three different things: the pin, whether
/// it is installed, and what would actually be used when there is no pin.
fn runtime_line(i: &Inspection) -> String {
    match (&i.runtime_pin, &i.runtime_resolved) {
        (Some(pin), _) => {
            let mark = if i.runtime_installed {
                "installed"
            } else {
                "NOT installed — run `cln install runtime <version>`"
            };
            format!("{pin} (pinned) — {mark}")
        }
        (None, Some(active)) => format!("{active} (active)"),
        (None, None) => "none installed — run `cln install runtime latest`".into(),
    }
}

// ---------------------------------------------------------------------------
// open
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// The package to open.
    pub path: PathBuf,
}

/// Show the package, then do whatever the user picks.
///
/// This is the verb the `.app` bundle invokes, so its failures have to be
/// visible without a terminal: anything that goes wrong is reported in a
/// dialog rather than only on stderr, which a double-click would discard.
pub fn open(args: OpenArgs, env: &Env) -> Result<ExitCode> {
    let i = match do_inspect(&args.path, &env.layout) {
        Ok(i) => i,
        Err(e) => {
            let msg = match e.remedy() {
                Some(r) => format!("{e}\n\n{r}"),
                None => e.to_string(),
            };
            crate::ui::error_dialog("This file could not be opened", &msg);
            return Ok(ExitCode::FAILURE);
        }
    };

    // A pinned runtime that is missing makes every action fail, so say it here
    // rather than after the user commits to one (P-2).
    if !i.runtime_installed {
        let want = i
            .runtime_pin
            .as_ref()
            .map(|v| format!("clean-runtime {v}"))
            .unwrap_or_else(|| "a Clean runtime".into());
        crate::ui::error_dialog(
            &format!("{} needs {want}", i.name),
            "It is not installed on this machine.\n\nInstall it with:\n    cln install runtime latest",
        );
        return Ok(ExitCode::FAILURE);
    }

    match crate::ui::choose_action(&i) {
        crate::ui::Action::Cancel => Ok(ExitCode::SUCCESS),
        crate::ui::Action::Details => {
            crate::ui::details_dialog(&i);
            Ok(ExitCode::SUCCESS)
        }
        crate::ui::Action::Run => crate::ui::run_in_terminal(&i, &env.layout),
        crate::ui::Action::Deploy => {
            // `cln deploy` does not exist yet. Saying so plainly is the whole
            // point: an button that silently did nothing would be worse than
            // one that explains what is missing.
            crate::ui::error_dialog(
                "Deploying to Clean Cloud is not available yet",
                "This build of the toolchain has no `cln deploy` command.\n\n\
                 You can still run the server locally from this window.",
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
