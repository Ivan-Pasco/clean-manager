//! Toolchain-version verbs — Manager §00.3.3.

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use cln_install::{install as do_install, seed_host_wit, uninstall as do_uninstall, VersionSpec};
use cln_shared::ToolchainKind;
use semver::Version;

use crate::env::{release_source_for, Env};

/// The kind flag as it appears on argv. Renders lowercase, per Manager
/// §00.3.3: `cln install compiler 1.2.0`.
#[derive(Copy, Clone, Debug, ValueEnum)]
#[value(rename_all = "lower")]
pub enum KindArg {
    Compiler,
    Framework,
    Runtime,
}

impl From<KindArg> for ToolchainKind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Compiler => ToolchainKind::Compiler,
            KindArg::Framework => ToolchainKind::Framework,
            KindArg::Runtime => ToolchainKind::Runtime,
        }
    }
}

impl KindArg {
    fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "compiler" => Some(KindArg::Compiler),
            "framework" => Some(KindArg::Framework),
            "runtime" => Some(KindArg::Runtime),
            _ => None,
        }
    }
}

/// Resolve an optional kind flag to the list of kinds to operate on.
/// None → all three (Manager §00.3.3: "defaults to installing all three").
fn kinds_from(arg: Option<KindArg>) -> Vec<ToolchainKind> {
    match arg {
        Some(k) => vec![k.into()],
        None => ToolchainKind::ALL.to_vec(),
    }
}

fn parse_spec(s: &str) -> Result<VersionSpec> {
    if s == "latest" {
        Ok(VersionSpec::Latest)
    } else {
        let v = Version::parse(s.trim_start_matches('v'))
            .with_context(|| format!("'{s}' is not a valid semver version"))?;
        Ok(VersionSpec::Exact(v))
    }
}

// ---------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Positional args: `[<kind>] <version>`.
    /// Clap can't have an optional positional before a required one, so both
    /// are captured here and disambiguated in [`resolve_install_positionals`].
    #[arg(value_names = ["KIND_OR_VERSION", "VERSION"], num_args = 1..=2)]
    positionals: Vec<String>,
    /// Install the version but do not switch the active symlink to it.
    #[arg(long)]
    pub no_activate: bool,
    /// Skip registering Clean file types with the OS (§00.12's opt-out).
    ///
    /// Registration is otherwise automatic, so a double-click works straight
    /// after install. `CLN_NO_REGISTER=1` does the same for scripted installs.
    #[arg(long)]
    pub no_register: bool,
}

fn resolve_install_positionals(pos: &[String]) -> Result<(Option<KindArg>, String)> {
    match pos {
        [version] => Ok((None, version.clone())),
        [kind, version] => {
            let parsed = KindArg::from_str_ci(kind).ok_or_else(|| {
                anyhow::anyhow!(
                    "'{kind}' is not a valid kind (expected: compiler, framework, runtime)"
                )
            })?;
            Ok((Some(parsed), version.clone()))
        }
        _ => Err(anyhow::anyhow!(
            "expected `<version>` or `<kind> <version>`"
        )),
    }
}

pub fn install(args: InstallArgs, env: &Env) -> Result<()> {
    let (kind_arg, version) = resolve_install_positionals(&args.positionals)?;
    let spec = parse_spec(&version)?;
    let activate = !args.no_activate;

    // Host contracts are toolchain-wide, not per-kind — seed once here rather
    // than letting a bare `cln install` report the same three files three
    // times. `do_install` seeds too (idempotently), so library callers that
    // bypass this verb still get a warm cache.
    env.layout.ensure_base().context("preparing ~/.cln")?;
    for c in seed_host_wit(&env.layout).context("seeding host contracts")? {
        if !c.already_present {
            println!("seeded host contract {}", c.label());
        }
    }

    for kind in kinds_from(kind_arg) {
        let source = release_source_for(kind);
        let outcome = do_install(&env.layout, source.as_ref(), &spec, env.platform, activate)
            .with_context(|| format!("installing {kind}"))?;
        if outcome.already_installed {
            println!("{kind} {} already installed", outcome.version);
        } else {
            println!("installed {kind} {}", outcome.version);
        }
        if outcome.activated {
            println!("active {kind} -> {}", outcome.version);
        }
    }

    // Bind .clapp/.serve so a double-click works straight after install,
    // rather than waiting for the user to discover a separate command
    // (§00.12: automatic, with an opt-out). Never fails the install.
    crate::verbs::os::register_after_install(env, args.no_register);

    Ok(())
}

// ---------------------------------------------------------------------------
// use
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct UseArgs {
    /// Positional args: `[<kind>] <version>`. See install for the reasoning.
    #[arg(value_names = ["KIND_OR_VERSION", "VERSION"], num_args = 1..=2)]
    positionals: Vec<String>,
}

pub fn use_(args: UseArgs, env: &Env) -> Result<()> {
    let (kind_arg, version) = resolve_install_positionals(&args.positionals)?;
    let v = Version::parse(version.trim_start_matches('v'))
        .with_context(|| format!("'{}' is not a valid semver version", version))?;
    for kind in kinds_from(kind_arg) {
        env.layout
            .set_active(kind, &v)
            .with_context(|| format!("switching active {kind}"))?;
        println!("active {kind} -> {v}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// uninstall
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct UninstallArgs {
    /// Which toolchain artifact to remove from.
    pub kind: KindArg,
    /// The version to remove.
    pub version: String,
}

pub fn uninstall(args: UninstallArgs, env: &Env) -> Result<()> {
    let kind: ToolchainKind = args.kind.into();
    let v = Version::parse(args.version.trim_start_matches('v'))
        .with_context(|| format!("'{}' is not a valid semver version", args.version))?;
    do_uninstall(&env.layout, kind, &v)?;
    println!("removed {kind} {v}");

    // §00.12 couples registration state to binary lifetime: an association
    // pointing at a runtime that is no longer installed would open a Terminal
    // only to print a resolution error. Removing the last runtime is the case
    // that actually breaks double-click, so deregister there rather than
    // leaving a association that cannot succeed.
    if kind == ToolchainKind::Runtime
        && env
            .layout
            .list_installed(ToolchainKind::Runtime)
            .map(|r| r.is_empty())
            .unwrap_or(false)
    {
        crate::verbs::os::unregister_after_last_runtime(env);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Show only one kind. Omit for all three.
    pub kind: Option<KindArg>,
}

pub fn list(args: ListArgs, env: &Env) -> Result<()> {
    for kind in kinds_from(args.kind) {
        let installed = env.layout.list_installed(kind)?;
        let active = env.layout.active_version(kind);
        println!("{kind}:");
        if installed.is_empty() {
            println!("  (none installed)");
            continue;
        }
        for v in installed {
            let marker = if Some(&v) == active.as_ref() {
                " (active)"
            } else {
                ""
            };
            println!("  {v}{marker}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// available
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct AvailableArgs {
    /// Show only one kind. Omit for all three.
    pub kind: Option<KindArg>,
}

pub fn available(args: AvailableArgs, env: &Env) -> Result<()> {
    for kind in kinds_from(args.kind) {
        let source = release_source_for(kind);
        let mut list = source
            .list(env.platform)
            .with_context(|| format!("listing releases for {kind}"))?;
        list.sort_by(|a, b| a.version.cmp(&b.version));
        println!("{kind}:");
        if list.is_empty() {
            println!("  (no releases found)");
            continue;
        }
        for entry in list {
            println!("  {}", entry.version);
        }
    }
    Ok(())
}
