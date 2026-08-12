//! Installation, activation, and removal of toolchain artifacts.
//!
//! Orchestrates a `ReleaseSource` (where to fetch releases from) with a
//! `Layout` (where they land on disk). The default source is
//! [`channels::GithubReleases`]; tests use [`channels::LocalDir`] to stay
//! off the network.
//!
//! Policy lives here — layout stays mechanical. In particular, `uninstall`
//! refuses to remove the currently active version (Manager §00.3.3).

pub mod channels;
pub mod download;
pub mod extract;
pub mod hostwit;
pub mod install;
pub mod repos;
pub mod uninstall;

pub use channels::{GithubReleases, LocalDir, ReleaseSource, VersionSpec};
pub use hostwit::{seed_all as seed_host_wit, Contract, SeedError, Seeded};
pub use install::{install, InstallError, InstallOutcome};
pub use uninstall::{uninstall, UninstallError};
