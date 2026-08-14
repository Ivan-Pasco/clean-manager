//! Shared per-invocation state: which `~/.cln/` root to use, the current
//! platform, and where to source releases from.

use anyhow::{anyhow, Context, Result};
use cln_layout::Layout;
use cln_shared::{Platform, ToolchainKind};

pub struct Env {
    pub layout: Layout,
    pub platform: Platform,
}

impl Env {
    pub fn detect() -> Result<Self> {
        // `CLN_HOME` is handled inside `from_home` rather than here, so that
        // every process resolving a toolchain — including the framework binary
        // this CLI dispatches to — agrees on one root.
        let layout = Layout::from_home().ok_or_else(|| {
            anyhow!("HOME is not set; cannot locate ~/.cln/ (set CLN_HOME to override)")
        })?;
        layout.ensure_base().context("preparing ~/.cln/ layout")?;
        let platform = Platform::detect().ok_or_else(|| {
            anyhow!(
                "unsupported platform: {} {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
        Ok(Self { layout, platform })
    }
}

/// Build the release source for a kind. Honors two overrides used by tests
/// and local development:
///
/// - `CLN_RELEASES_DIR=/path` — a shared `LocalDir` root; each kind reads
///   from `<CLN_RELEASES_DIR>/<kind>/` (e.g. `/tmp/rel/compiler/1.0.0/...`).
/// - `CLN_<KIND>_REPO=owner/repo` — override the GitHub repo (in `cln-install::repos`).
pub fn release_source_for(kind: ToolchainKind) -> Box<dyn cln_install::ReleaseSource> {
    if let Ok(dir) = std::env::var("CLN_RELEASES_DIR") {
        let sub = std::path::PathBuf::from(dir).join(kind.as_str());
        return Box::new(cln_install::LocalDir::new(kind, sub));
    }
    Box::new(cln_install::GithubReleases::new(kind))
}
