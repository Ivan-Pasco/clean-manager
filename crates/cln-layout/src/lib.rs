//! The `~/.cln/` on-disk layout, per Manager §00.2.
//!
//! Every read and write under `~/.cln/` goes through this crate. Nothing else
//! is allowed to string-format a path under the layout — refactoring the tree
//! later should touch one file, not fifty.
//!
//! MGR-02 (bounded on-disk footprint) is enforced structurally: [`Layout`] is
//! the only handle, it's rooted at a single directory, and every path
//! accessor returns a child of that root.

pub mod paths;
pub mod versions;
pub mod active;

pub use paths::Layout;
