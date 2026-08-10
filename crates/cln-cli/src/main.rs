//! `cln` — the single front door for the Clean Language toolchain (Manager §00.1).
//!
//! M0 verb surface: `install`, `use`, `uninstall`, `list`, `available`,
//! plus `--version`. Every other verb is deferred to M1+ per PLAN.md §4.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod env;
mod verbs;

/// Every user-visible argv shape lives here.
#[derive(Parser, Debug)]
#[command(
    name = "cln",
    version,
    about = "The Clean Language toolchain.",
    long_about = "The single developer-facing binary for the Clean Language.\n\
                  Installs, pins, and switches compiler / framework / runtime versions.\n\
                  See `cln <verb> --help` for details."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Install a toolchain artifact (compiler, framework, runtime).
    ///
    /// Kind is optional and defaults to all three at the same version.
    /// `latest` resolves to the newest published stable release.
    Install(verbs::toolchain::InstallArgs),

    /// Switch the globally active version for a toolchain kind.
    Use(verbs::toolchain::UseArgs),

    /// Remove an installed version.
    /// Fails if it is the active version.
    Uninstall(verbs::toolchain::UninstallArgs),

    /// List installed toolchain versions.
    List(verbs::toolchain::ListArgs),

    /// List versions available for install from the release channel.
    Available(verbs::toolchain::AvailableArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let env = match env::Env::detect() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cln: {e}");
            return ExitCode::from(2);
        }
    };
    let result = match cli.command {
        Cmd::Install(a) => verbs::toolchain::install(a, &env),
        Cmd::Use(a) => verbs::toolchain::use_(a, &env),
        Cmd::Uninstall(a) => verbs::toolchain::uninstall(a, &env),
        Cmd::List(a) => verbs::toolchain::list(a, &env),
        Cmd::Available(a) => verbs::toolchain::available(a, &env),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cln: {e}");
            ExitCode::from(1)
        }
    }
}
