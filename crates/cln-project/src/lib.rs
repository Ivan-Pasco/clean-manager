//! Per-project state under a project's own `.cln/` directory.
//!
//! Two responsibilities, both small and both read far more often than written:
//!
//! - [`discover`] — find the project root by walking up from a starting
//!   directory looking for `clean.toml`.
//! - [`pins`] — read and write `.cln/version`, `.cln/frame-version`, and
//!   `.cln/runtime-version`, the per-project overrides of the globally active
//!   toolchain versions (PLAN.md §4 Phase 2).
//!
//! **This crate does not read `clean.toml`.** Project configuration is the
//! framework's to parse; manager only needs the file's *location* to know
//! where the project root is. Keeping it that way is what stops manager from
//! growing a second, divergent config parser.

pub mod discover;
pub mod pins;

pub use discover::{find_project_root, Project, ProjectError};
pub use pins::{pin_file, Pins, PinsError};
