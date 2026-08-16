//! `cln run <path>` — Manager §00.13.
//!
//! Thin argv → library call, like every other verb. The decisions live in
//! `cln-run`; this file parses flags, optionally reports the plan, and
//! propagates the guest's exit code.
//!
//! **Nothing is printed on the success path.** The guest's stdout is the
//! user's, and `clean-cli` guarantees no framing of its own (CLIH-10) — so
//! manager adds none either. A `cln run` that printed "running…" would corrupt
//! the output of every guest whose stdout is piped into something.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Result};
use clap::Args;
use cln_project::Project;
use cln_run::{exit_code, invoke, plan, Options};

use crate::env::Env;

#[derive(Args, Debug)]
pub struct RunArgs {
    /// The artifact to run: a `.clapp` bundle or a `.wasm` component.
    pub path: PathBuf,

    /// Which world to run, for a bundle that declares several.
    #[arg(long, value_name = "WORLD")]
    pub world: Option<String>,

    /// Host configuration to use instead of the artifact's own.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Explain which runtime was chosen and why.
    #[arg(long, short)]
    pub verbose: bool,

    /// Arguments passed to the guest, after `--`.
    #[arg(last = true)]
    pub guest_args: Vec<String>,
}

pub fn run(args: RunArgs, env: &Env) -> Result<ExitCode> {
    let opts = Options {
        world: args.world,
        config: args.config,
        guest_args: args.guest_args,
        // A project pin only applies when the user is standing in a project.
        // Absence is normal — a `.clapp` is usually run from anywhere — so a
        // failed discovery is not an error here.
        project_root: Project::discover(".").ok().map(|p| p.root().to_path_buf()),
    };

    let (plan, invocation) =
        plan(&args.path, &opts, &env.layout).map_err(|e| annotate(e.remedy(), e))?;

    if args.verbose {
        // stderr, so a guest whose stdout is being piped stays clean.
        if let Some(package) = &plan.package {
            eprintln!("cln: running {package}");
        }
        eprintln!(
            "cln: runtime {} ({}) from {}",
            plan.runtime.version,
            plan.runtime.source.as_str(),
            plan.runtime.binary.display()
        );
        eprintln!("cln: world {}", plan.world);
        eprintln!("cln: component {}", plan.wasm.display());
        eprintln!("cln: config {}", plan.config.display());
    }

    let code = invoke(&invocation).map_err(|e| annotate(e.remedy(), e))?;
    Ok(exit_code(code))
}

/// Attach a remedy line, matching how every other verb coaches.
fn annotate<E: std::error::Error + Send + Sync + 'static>(
    remedy: Option<String>,
    error: E,
) -> anyhow::Error {
    match remedy {
        Some(r) => anyhow!("{error}\n  help: {r}"),
        None => anyhow!(error),
    }
}
