//! Project verbs that manager forwards to the framework (Manager §00.4).
//!
//! `cln build` and `cln package` do no building. They locate the project,
//! decide which framework version it wants, spawn that binary with the verb the
//! user typed, and render what comes back. Every line of actual build logic
//! lives in the framework.

use std::process::ExitCode;

use anyhow::{anyhow, Result};
use clap::Args;
use cln_dispatch::{
    dispatch,
    envelope::{Envelope, EnvelopeError},
    resolve_component, route, VersionSource,
};
use cln_project::Project;

use crate::env::Env;
use crate::output::{render_envelope, render_summary, Style};

#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Project directory. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub path: std::path::PathBuf,

    /// Audited config override, `section.key=value`.
    #[arg(long = "override", value_name = "PATH=VALUE")]
    pub overrides: Vec<String>,

    /// Shorthand for `--override build.optimization=<value>`.
    #[arg(long)]
    pub optimization: Option<String>,

    /// Refuse network operations; use the local cache or fail.
    #[arg(long)]
    pub offline: bool,

    /// Print the component's raw JSON envelope instead of a rendered summary.
    /// For scripts and CI.
    #[arg(long)]
    pub json: bool,

    /// Explain which component binary was chosen and why.
    #[arg(long, short)]
    pub verbose: bool,
}

/// `cln build` — compile the project to `dist/app.wasm`.
pub fn build(args: BuildArgs, env: &Env) -> Result<ExitCode> {
    forward("build", args, env)
}

/// `cln package` — wrap the built component into a distributable archive.
pub fn package(args: BuildArgs, env: &Env) -> Result<ExitCode> {
    forward("package", args, env)
}

/// The shared path both verbs take: locate → resolve → spawn → render.
fn forward(verb: &str, args: BuildArgs, env: &Env) -> Result<ExitCode> {
    let route = route(verb).ok_or_else(|| anyhow!("`{verb}` is not a dispatched verb"))?;

    // Locate the project first: a missing clean.toml is the user's most likely
    // mistake, and saying so beats a framework error about a path.
    let project = Project::discover(&args.path)?;

    let resolved = resolve_component(route.component, Some(project.root()), &env.layout)
        .map_err(|e| annotate(e.remedy(), e))?;

    if args.verbose {
        eprintln!(
            "cln: using {} {} ({}) from {}",
            resolved.kind,
            resolved.version,
            resolved.source.as_str(),
            resolved.binary.display()
        );
        if resolved.source == VersionSource::Pin {
            eprintln!("cln: pinned by {}", project.cln_dir().display());
        }
    }

    let argv = child_argv(route.forwards_as, &project, &args);
    let child_env = child_env(&args);

    let outcome = dispatch(&resolved.binary, &argv, Some(project.root()), &child_env)
        .map_err(|e| annotate(e.remedy(), e))?;

    render(&outcome.stdout, outcome.code, &args)?;

    // The component's exit code is the user's exit code, unchanged.
    Ok(exit_code(outcome.code))
}

/// Build the argv for the component, minus the `cln` prefix.
fn child_argv(forwards_as: &str, project: &Project, args: &BuildArgs) -> Vec<String> {
    let mut argv = vec![forwards_as.to_string()];

    // Pass the resolved project root rather than the user's path: the framework
    // then works from the same directory manager decided on, even when the user
    // typed a nested path or a relative one.
    argv.push(project.root().display().to_string());

    for o in &args.overrides {
        argv.push("--override".into());
        argv.push(o.clone());
    }
    if let Some(opt) = &args.optimization {
        argv.push("--optimization".into());
        argv.push(opt.clone());
    }
    if args.offline {
        argv.push("--offline".into());
    }
    argv
}

/// Environment for the child. `--offline` propagates as `CLN_OFFLINE` so any
/// process the framework spawns in turn inherits the same restriction
/// (PLAN.md §6 open question 8).
fn child_env(args: &BuildArgs) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    if args.offline {
        env.push(("CLN_OFFLINE", "1".to_string()));
    }
    env
}

/// Show the user the result: raw envelope with `--json`, rendered otherwise.
///
/// **Why diagnostics are not re-rendered by default.** PLAN.md §3 makes manager
/// the toolchain's single diagnostic renderer, but `clean-framework` 0.1.1 also
/// prints its diagnostics to stderr on the way out. Rendering the envelope's
/// copy as well shows the user every error twice. Until the framework grows a
/// flag to stay quiet, manager prints the outcome summary and leaves the
/// diagnostic text to the component that already emitted it.
///
/// `--verbose` opts into manager's own rendering, which is the richer one —
/// it lays out spans, labels, and doc URLs from the structured envelope.
fn render(stdout: &str, code: i32, args: &BuildArgs) -> Result<()> {
    if args.json {
        // Pass the component's bytes through untouched — a script parsing this
        // should see exactly what the component emitted.
        print!("{stdout}");
        if !stdout.ends_with('\n') {
            println!();
        }
        return Ok(());
    }

    match Envelope::parse(stdout) {
        Ok(env) => {
            let style = Style::detect();
            let rendered = if args.verbose {
                render_envelope(&env, style)
            } else {
                render_summary(&env, style)
            };
            if !rendered.is_empty() {
                eprintln!("{rendered}");
            }
            Ok(())
        }
        // A component that failed before it could emit an envelope has already
        // explained itself on stderr; adding a parse error would only bury it.
        Err(EnvelopeError::Empty) if code != 0 => Ok(()),
        Err(e) => Err(anyhow!(e)),
    }
}

/// Attach a remedy line to an error, so failures name the command that fixes
/// them.
fn annotate<E: std::error::Error + Send + Sync + 'static>(
    remedy: Option<String>,
    error: E,
) -> anyhow::Error {
    match remedy {
        Some(r) => anyhow!("{error}\n  help: {r}"),
        None => anyhow!(error),
    }
}

/// Map a child's exit status onto a process exit code.
fn exit_code(code: i32) -> ExitCode {
    // ExitCode is a u8; anything outside that range (or a signal death) is
    // reported as a generic failure rather than wrapping around to 0.
    match u8::try_from(code) {
        Ok(c) => ExitCode::from(c),
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cln_shared::ToolchainKind as Kind;

    /// The verbs this module forwards. Kept beside the tests that assert the
    /// routing table agrees with them.
    const DISPATCHED_VERBS: &[&str] = &["build", "package"];

    fn args(path: &str) -> BuildArgs {
        BuildArgs {
            path: path.into(),
            overrides: Vec::new(),
            optimization: None,
            offline: false,
            json: false,
            verbose: false,
        }
    }

    #[test]
    fn argv_starts_with_the_verb_and_the_project_root() {
        let p = Project::at("/projects/demo");
        let argv = child_argv("build", &p, &args("."));
        assert_eq!(argv[0], "build");
        assert_eq!(argv[1], "/projects/demo");
    }

    #[test]
    fn argv_forwards_overrides_and_flags() {
        let p = Project::at("/projects/demo");
        let mut a = args(".");
        a.overrides = vec!["build.optimization=size".into()];
        a.optimization = Some("speed".into());
        a.offline = true;

        let argv = child_argv("build", &p, &a);
        assert!(argv.contains(&"--override".to_string()));
        assert!(argv.contains(&"build.optimization=size".to_string()));
        assert!(argv.contains(&"--optimization".to_string()));
        assert!(argv.contains(&"speed".to_string()));
        assert!(argv.contains(&"--offline".to_string()));
    }

    #[test]
    fn argv_omits_flags_that_were_not_given() {
        let p = Project::at("/projects/demo");
        let argv = child_argv("build", &p, &args("."));
        assert_eq!(argv.len(), 2, "verb + path only: {argv:?}");
    }

    #[test]
    fn package_forwards_under_its_own_verb() {
        let p = Project::at("/projects/demo");
        assert_eq!(child_argv("package", &p, &args("."))[0], "package");
    }

    #[test]
    fn offline_propagates_as_an_environment_variable() {
        let mut a = args(".");
        assert!(child_env(&a).is_empty());
        a.offline = true;
        assert_eq!(child_env(&a), vec![("CLN_OFFLINE", "1".to_string())]);
    }

    #[test]
    fn every_dispatched_verb_has_a_route_to_the_framework() {
        for verb in DISPATCHED_VERBS {
            let r = route(verb).unwrap_or_else(|| panic!("{verb} must be routed"));
            assert_eq!(r.component, Kind::Framework);
        }
    }

    #[test]
    fn exit_codes_survive_the_round_trip() {
        // The framework's 0/1/2 are the codes users and CI branch on.
        for code in [0u8, 1, 2, 101] {
            assert_eq!(
                format!("{:?}", exit_code(code as i32)),
                format!("{:?}", ExitCode::from(code))
            );
        }
    }

    #[test]
    fn an_out_of_range_code_becomes_a_generic_failure() {
        // 128+9 fits in u8, but a hypothetical larger value must not wrap to 0.
        assert_eq!(
            format!("{:?}", exit_code(4096)),
            format!("{:?}", ExitCode::FAILURE)
        );
    }
}
