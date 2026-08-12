//! Types shared across the Clean toolchain.
//!
//! Everything in this crate is wire-visible: it appears in JSON, TOML, or
//! subprocess argv shared between manager, framework, and compiler. Adding a
//! type here is a coordination event — bump all three components together.

pub mod channel;
pub mod kind;
pub mod platform;

pub use channel::{Compatibility, ReleaseEntry};
pub use kind::ToolchainKind;
pub use platform::Platform;

pub use semver::{Version, VersionReq};
