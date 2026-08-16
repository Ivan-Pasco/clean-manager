//! `cln` — the single front door for the Clean Language toolchain (Manager §00.1).
//!
//! Verb surface: the toolchain verbs `install`, `use`, `uninstall`, `list`,
//! `available`, plus the dispatched project verbs `build` and `package`
//! (PLAN.md §4 Phase 2). Every other verb is deferred per PLAN.md §4.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod env;
mod output;
mod ui;
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

    /// Build a project into dist/app.wasm.
    ///
    /// Dispatches to the framework version this project pins, falling back to
    /// the globally active one.
    Build(verbs::project::BuildArgs),

    /// Package a built project into a distributable archive.
    Package(verbs::project::BuildArgs),

    /// Show what a package is: kind, version, runtime, signature.
    Inspect(verbs::pkg::InspectArgs),

    /// Open a package the way a double-click does: show it, then offer
    /// the actions valid for its kind.
    Open(verbs::pkg::OpenArgs),

    /// Register Clean file types with the OS so double-click runs them.
    ///
    /// Runs automatically during `cln install`; this is for re-running it or
    /// inspecting the result with `--status`.
    Register(verbs::os::RegisterArgs),

    /// Remove Clean file associations from the OS.
    Unregister(verbs::os::UnregisterArgs),

    /// Run a packaged artifact.
    ///
    /// Accepts a `.clapp` bundle or a bare `.wasm` component. The runtime is
    /// chosen by the artifact's pin, then the project's, then the active one.
    Run(verbs::run::RunArgs),
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
    // Manager's own verbs succeed or fail; dispatched verbs carry back the
    // component's exit code, which must reach the shell unchanged so scripts
    // can branch on the framework's 0 / 1 / 2.
    let result = match cli.command {
        Cmd::Install(a) => verbs::toolchain::install(a, &env).map(|()| ExitCode::SUCCESS),
        Cmd::Use(a) => verbs::toolchain::use_(a, &env).map(|()| ExitCode::SUCCESS),
        Cmd::Uninstall(a) => verbs::toolchain::uninstall(a, &env).map(|()| ExitCode::SUCCESS),
        Cmd::List(a) => verbs::toolchain::list(a, &env).map(|()| ExitCode::SUCCESS),
        Cmd::Available(a) => verbs::toolchain::available(a, &env).map(|()| ExitCode::SUCCESS),
        Cmd::Build(a) => verbs::project::build(a, &env),
        Cmd::Package(a) => verbs::project::package(a, &env),
        Cmd::Run(a) => verbs::run::run(a, &env),
        Cmd::Inspect(a) => verbs::pkg::inspect(a, &env),
        Cmd::Open(a) => verbs::pkg::open(a, &env),
        Cmd::Register(a) => verbs::os::register(a, &env).map(|()| ExitCode::SUCCESS),
        Cmd::Unregister(a) => verbs::os::unregister(a, &env).map(|()| ExitCode::SUCCESS),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cln: {e}");
            ExitCode::from(1)
        }
    }
}
