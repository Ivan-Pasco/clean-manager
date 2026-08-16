//! `~/.cln/registrations/state.toml` — what manager has registered with the OS.
//!
//! This file is what makes registration *idempotent* and *removable*. The OS
//! side is the source of truth for whether an association works; this file is
//! the record of what manager put there, so `unregister` knows what to take
//! back and `--status` can report drift when something else claims the type.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use cln_layout::Layout;
use serde::{Deserialize, Serialize};

/// A file extension manager is allowed to claim.
///
/// This is a closed enum rather than a string on purpose. §00.12 forbids
/// claiming `.wasm` "under any circumstance" — a shared format owned by the
/// wider WebAssembly ecosystem — and the cheapest way to keep a future caller
/// from passing `"wasm"` into a registration routine is to make the type
/// system refuse to represent it.
///
/// **There is one compiled extension.** `.serve` was retired by §00.14 P-1:
/// both kinds of package are `.clapp`, and `manifest.toml`'s `kind` field
/// distinguishes an application from a server bundle. The extension was never
/// the discriminator — `cln run` and `cln open` both read the manifest — so a
/// second extension carried the same information twice and doubled the format
/// to version.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Extension {
    /// A Clean package: an application or a server bundle (§00.14).
    Clapp,
}

impl Extension {
    /// Every extension manager registers. `.wasm` and `.cln` are deliberately
    /// absent — see the type docs and §00.12's table.
    pub const ALL: [Extension; 1] = [Extension::Clapp];

    /// The extension without its leading dot, as it appears on disk.
    pub fn as_str(self) -> &'static str {
        match self {
            Extension::Clapp => "clapp",
        }
    }

    /// The Uniform Type Identifier manager declares for this extension.
    ///
    /// Reverse-DNS under a domain the project controls, so it cannot collide
    /// with a UTI another vendor declares.
    pub fn uti(self) -> &'static str {
        match self {
            Extension::Clapp => "dev.cleanlanguage.clapp",
        }
    }

    /// A human-readable description, shown by Finder in the Kind column.
    pub fn description(self) -> &'static str {
        match self {
            Extension::Clapp => "Clean Package",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "clapp" => Some(Extension::Clapp),
            _ => None,
        }
    }
}

impl fmt::Display for Extension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ".{}", self.as_str())
    }
}

/// One extension's registration record.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub registered: bool,
    /// The user ran `cln unregister`, so `cln install` MUST NOT re-register.
    ///
    /// §00.12: an install that silently undid an explicit `cln unregister`
    /// would make the opt-out meaningless — the user would have to decline
    /// again after every upgrade. Recorded per-extension alongside
    /// `registered` so the two always round-trip together; a later
    /// `cln register` clears it, because asking for it back is unambiguous.
    #[serde(default, skip_serializing_if = "is_false")]
    pub declined: bool,
    /// What was created on the OS side — the `.app` bundle path on macOS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_path: Option<PathBuf>,
    /// The `cln` binary the association invokes. Recorded so `--status` can
    /// notice a registration left behind by a toolchain that has since moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_binary: Option<PathBuf>,
    /// RFC 3339, supplied by the caller. This crate does not read the clock:
    /// a timestamp is not worth a dependency, and taking it as a parameter
    /// keeps the state file deterministic under test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Record {
    /// The record left behind by `cln unregister`: not registered, and
    /// explicitly declined so a later install does not undo the choice.
    pub fn declined_record() -> Self {
        Record {
            registered: false,
            declined: true,
            ..Default::default()
        }
    }
}

/// The whole `state.toml`, keyed by extension name (`clapp`, `serve`).
///
/// A map rather than named fields so an extension added later reads back on an
/// older binary instead of failing to parse — and `BTreeMap` rather than
/// `HashMap` so the serialized file is byte-deterministic (PLAN.md §5).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct State {
    pub entries: BTreeMap<String, Record>,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not serialize registration state: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl State {
    pub fn get(&self, ext: Extension) -> Option<&Record> {
        self.entries.get(ext.as_str())
    }

    pub fn set(&mut self, ext: Extension, record: Record) {
        self.entries.insert(ext.as_str().to_string(), record);
    }

    pub fn remove(&mut self, ext: Extension) {
        self.entries.remove(ext.as_str());
    }

    /// True when this extension is recorded as registered.
    pub fn is_registered(&self, ext: Extension) -> bool {
        self.get(ext).is_some_and(|r| r.registered)
    }

    /// True when the user explicitly unregistered this extension.
    ///
    /// `cln install` consults this before auto-registering, so a decision the
    /// user made once is not undone by the next upgrade.
    pub fn is_declined(&self, ext: Extension) -> bool {
        self.get(ext).is_some_and(|r| r.declined)
    }

    /// True when the user has declined every extension — the state an install
    /// must treat as "do not register".
    pub fn declined_all(&self) -> bool {
        Extension::ALL.iter().all(|e| self.is_declined(*e))
    }
}

/// `~/.cln/registrations/`.
pub fn dir(layout: &Layout) -> PathBuf {
    layout.root().join("registrations")
}

/// `~/.cln/registrations/state.toml`.
pub fn path(layout: &Layout) -> PathBuf {
    dir(layout).join("state.toml")
}

/// Read the state file. A missing file is an empty state, not an error: a
/// machine that has never registered anything is the normal starting point.
pub fn load(layout: &Layout) -> Result<State, StateError> {
    let p = path(layout);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(source) => return Err(StateError::Read { path: p, source }),
    };
    toml::from_str(&text).map_err(|source| StateError::Parse { path: p, source })
}

/// Write the state file, creating `~/.cln/registrations/` if needed.
pub fn save(layout: &Layout, state: &State) -> Result<(), StateError> {
    let d = dir(layout);
    std::fs::create_dir_all(&d).map_err(|source| StateError::Write { path: d, source })?;
    let p = path(layout);
    let text = toml::to_string_pretty(state)?;
    std::fs::write(&p, text).map_err(|source| StateError::Write { path: p, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn layout() -> (tempfile::TempDir, Layout) {
        let home = tempdir().unwrap();
        let l = Layout::new(home.path());
        l.ensure_base().unwrap();
        (home, l)
    }

    #[test]
    fn a_machine_that_never_registered_reads_as_empty() {
        let (_h, l) = layout();
        let s = load(&l).unwrap();
        assert!(s.entries.is_empty());
        assert!(!s.is_registered(Extension::Clapp));
    }

    #[test]
    fn state_round_trips() {
        let (_h, l) = layout();
        let mut s = State::default();
        s.set(
            Extension::Clapp,
            Record {
                registered: true,
                declined: false,
                os_path: Some(PathBuf::from("/Users/a/Applications/Clean.app")),
                bound_binary: Some(PathBuf::from("/Users/a/.cln/bin/cln")),
                registered_at: Some("2026-08-16T00:00:00Z".into()),
            },
        );
        save(&l, &s).unwrap();

        let back = load(&l).unwrap();
        assert_eq!(back, s);
        assert!(back.is_registered(Extension::Clapp));
    }

    /// PLAN.md §5 requires deterministic writes; a map iteration order that
    /// varied would produce a spurious diff on every save.
    #[test]
    fn writes_are_byte_deterministic() {
        let (_h, l) = layout();
        let mut s = State::default();
        for ext in Extension::ALL {
            s.set(
                ext,
                Record {
                    registered: true,
                    declined: false,
                    os_path: Some(PathBuf::from("/tmp/Clean.app")),
                    bound_binary: Some(PathBuf::from("/tmp/cln")),
                    registered_at: Some("2026-08-16T00:00:00Z".into()),
                },
            );
        }
        save(&l, &s).unwrap();
        let first = std::fs::read_to_string(path(&l)).unwrap();
        save(&l, &s).unwrap();
        let second = std::fs::read_to_string(path(&l)).unwrap();
        assert_eq!(first, second);
    }

    /// An extension a newer manager wrote must not stop an older one from
    /// reading the extensions it does know.
    #[test]
    fn an_unknown_extension_in_the_file_is_preserved_not_fatal() {
        let (_h, l) = layout();
        std::fs::create_dir_all(dir(&l)).unwrap();
        std::fs::write(
            path(&l),
            "[clapp]\nregistered = true\n\n[future]\nregistered = true\n",
        )
        .unwrap();

        let s = load(&l).unwrap();
        assert!(s.is_registered(Extension::Clapp));
        assert!(s.entries.contains_key("future"));
    }

    #[test]
    fn wasm_can_never_be_named_as_an_extension() {
        assert!(Extension::parse("wasm").is_none());
        assert!(Extension::parse(".wasm").is_none());
        assert!(Extension::parse("cln").is_none());
        assert_eq!(Extension::parse(".clapp"), Some(Extension::Clapp));
        // .serve was retired by §00.14 P-1; it must no longer parse.
        assert!(Extension::parse("serve").is_none());
        assert!(Extension::parse(".serve").is_none());
    }

    #[test]
    fn a_corrupt_state_file_names_the_path() {
        let (_h, l) = layout();
        std::fs::create_dir_all(dir(&l)).unwrap();
        std::fs::write(path(&l), "this is not toml {{{").unwrap();

        let err = load(&l).unwrap_err();
        assert!(matches!(err, StateError::Parse { .. }));
        assert!(err.to_string().contains("state.toml"));
    }
}
