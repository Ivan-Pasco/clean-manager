//! Dispatching a `cln` verb to the component binary that implements it
//! (Manager §00.4).
//!
//! Manager never builds, compiles, or runs wasm. For verbs like `cln build` it
//! resolves *which* component binary to launch, spawns it with the argv the
//! user typed minus the `cln` prefix, streams its output, and propagates its
//! exit code. That is the entire job, and keeping it in one crate is what lets
//! us swap subprocess dispatch for in-process linking later without touching
//! any verb (PLAN.md §2).
//!
//! The four pieces:
//!
//! - [`table`] — which verb goes to which component.
//! - [`resolve`] — which *version* of that component, pin before global active.
//! - [`stream`] — spawn, stream stderr live, capture stdout, propagate exit.
//! - [`envelope`] — the JSON contract the framework speaks on stdout.

pub mod envelope;
pub mod resolve;
pub mod stream;
pub mod table;

pub use envelope::{Diagnostic, Envelope, EnvelopeError, Severity};
pub use resolve::{resolve_component, ResolveError, Resolved, VersionSource};
pub use stream::{dispatch, DispatchError, Outcome};
pub use table::{route, Route};
