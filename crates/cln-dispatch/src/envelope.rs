//! The JSON envelope a dispatched component writes to stdout.
//!
//! The output contract (framework `framework-cli/src/verbs.rs`, PLAN.md §3):
//! human-readable progress goes to **stderr**, and **stdout** carries exactly
//! one JSON object describing the outcome. Manager parses it and renders the
//! diagnostics itself, so the toolchain has one diagnostic renderer at the top
//! rather than one per component.
//!
//! ```text
//! {"status":"ok","dist_wasm":"dist/app.wasm","wasm_sha256":"…","diagnostics":[]}
//! {"status":"error","diagnostics":[{"level":"error","code":"CFG005",…}]}
//! ```
//!
//! **Unknown fields are kept, not rejected.** The framework adds envelope keys
//! as it grows (`package_sha256`, `rebuilt`, …). A manager that refused to
//! parse an envelope containing a key it did not know would break every user's
//! build the day the framework shipped a new field, so the typed struct holds
//! only what manager renders and [`Envelope::extra`] carries the rest.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A diagnostic in the Platform 13 §2 wire shape.
///
/// Field-for-field compatible with the framework's `Diagnostic`, including its
/// `skip_serializing_if` defaults, so an envelope round-trips unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: Severity,
    /// `PREFIX###`, resolving to a row in Platform 09.
    pub code: String,
    pub message: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_span: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary: Vec<Annotation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_url: Option<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Help,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Help => "help",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Position {
    /// 1-based, counted in characters.
    pub line: u32,
    /// 1-based, counted in characters.
    pub column: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// Project-relative, forward-slashed.
    pub file: String,
    pub start: Position,
    /// Exclusive end.
    pub end: Position,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub span: Span,
    pub label: String,
}

/// The parsed stdout envelope.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Envelope {
    /// `"ok"` or `"error"`. Kept as a string rather than an enum because an
    /// unrecognized status must not make the envelope unparseable — we still
    /// want its diagnostics.
    #[serde(default)]
    pub status: String,

    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,

    /// The framework version that produced this envelope.
    #[serde(default)]
    pub framework_version: Option<String>,

    /// Every other key, preserved verbatim for `--json` and for rendering
    /// outcome fields (`dist_wasm`, `package`, `wasm_sha256`, …).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("the component produced no output envelope on stdout")]
    Empty,

    #[error("could not parse the component's output envelope: {source}")]
    Malformed {
        #[source]
        source: serde_json::Error,
        raw: String,
    },
}

impl Envelope {
    /// Parse the captured stdout of a dispatched component.
    ///
    /// Tolerates surrounding whitespace and, defensively, extra lines around
    /// the JSON object: the contract says stdout carries exactly one envelope,
    /// but a component that leaks a stray `println!` should degrade to a
    /// rendered build rather than a parse failure.
    pub fn parse(stdout: &str) -> Result<Self, EnvelopeError> {
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Err(EnvelopeError::Empty);
        }

        match serde_json::from_str::<Self>(trimmed) {
            Ok(envelope) => Ok(envelope),
            Err(first) => {
                // Fall back to the last line that parses as an object — the
                // envelope is printed last, after any stray output.
                for line in trimmed.lines().rev() {
                    let line = line.trim();
                    if !line.starts_with('{') {
                        continue;
                    }
                    if let Ok(envelope) = serde_json::from_str::<Self>(line) {
                        return Ok(envelope);
                    }
                }
                Err(EnvelopeError::Malformed {
                    source: first,
                    raw: trimmed.to_string(),
                })
            }
        }
    }

    /// Whether the component reported success.
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    /// A string-valued extra field, if present.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(|v| v.as_str())
    }

    /// Diagnostics at error level.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.level == Severity::Error)
    }

    /// Diagnostics at warning level.
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.level == Severity::Warning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-for-byte the success envelope `clean-framework build` prints.
    const BUILD_OK: &str = r#"{"status":"ok","dist_wasm":"dist/app.wasm","build_manifest":"dist/build-manifest.json","request_sha256":"aaa","wasm_sha256":"bbb","diagnostics":[],"framework_version":"0.1.1"}"#;

    /// The failure envelope, with a spanned compiler diagnostic.
    const BUILD_ERR: &str = r#"{"status":"error","diagnostics":[{"level":"error","code":"CFG005","message":"invalid UTF-8 in clean.toml","notes":["the file must be UTF-8"],"helps":["re-save the file as UTF-8"],"primary_span":{"file":"clean.toml","start":{"line":3,"column":1},"end":{"line":3,"column":9}}}],"framework_version":"0.1.1"}"#;

    #[test]
    fn parses_the_build_success_envelope() {
        let e = Envelope::parse(BUILD_OK).unwrap();
        assert!(e.is_ok());
        assert_eq!(e.field("dist_wasm"), Some("dist/app.wasm"));
        assert_eq!(e.field("wasm_sha256"), Some("bbb"));
        assert_eq!(e.framework_version.as_deref(), Some("0.1.1"));
        assert!(e.diagnostics.is_empty());
    }

    #[test]
    fn parses_the_failure_envelope_with_spans() {
        let e = Envelope::parse(BUILD_ERR).unwrap();
        assert!(!e.is_ok());
        assert_eq!(e.errors().count(), 1);

        let d = &e.diagnostics[0];
        assert_eq!(d.code, "CFG005");
        assert_eq!(d.level, Severity::Error);
        assert_eq!(d.notes.len(), 1);
        assert_eq!(d.helps.len(), 1);

        let span = d.primary_span.as_ref().unwrap();
        assert_eq!(span.file, "clean.toml");
        assert_eq!(span.start.line, 3);
        assert_eq!(span.end.column, 9);
    }

    #[test]
    fn parses_the_package_envelope() {
        let raw = r#"{"status":"ok","package":"dist/demo.clapp","package_sha256":"ccc","kind":"clapp","rebuilt":true,"framework_version":"0.1.1"}"#;
        let e = Envelope::parse(raw).unwrap();
        assert!(e.is_ok());
        assert_eq!(e.field("package"), Some("dist/demo.clapp"));
        assert_eq!(e.extra.get("rebuilt").unwrap(), &serde_json::json!(true));
    }

    /// The framework will add envelope fields; manager must not break when it
    /// does. This is the regression test for that promise.
    #[test]
    fn unknown_fields_are_preserved_rather_than_rejected() {
        let raw = r#"{"status":"ok","dist_wasm":"a.wasm","some_future_field":{"nested":1},"framework_version":"9.9.9"}"#;
        let e = Envelope::parse(raw).unwrap();
        assert!(e.is_ok());
        assert_eq!(
            e.extra.get("some_future_field").unwrap(),
            &serde_json::json!({"nested": 1})
        );
    }

    #[test]
    fn a_diagnostic_with_only_required_fields_parses() {
        let raw = r#"{"status":"error","diagnostics":[{"level":"warning","code":"W001","message":"unused"}]}"#;
        let e = Envelope::parse(raw).unwrap();
        assert_eq!(e.warnings().count(), 1);
        let d = &e.diagnostics[0];
        assert!(d.primary_span.is_none());
        assert!(d.notes.is_empty());
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let e = Envelope::parse(&format!("\n  {BUILD_OK}  \n\n")).unwrap();
        assert!(e.is_ok());
    }

    #[test]
    fn empty_stdout_is_a_distinct_error() {
        assert!(matches!(
            Envelope::parse("   \n"),
            Err(EnvelopeError::Empty)
        ));
    }

    #[test]
    fn non_json_stdout_is_malformed_and_keeps_the_raw_text() {
        let err = Envelope::parse("Segmentation fault").unwrap_err();
        match err {
            EnvelopeError::Malformed { raw, .. } => assert_eq!(raw, "Segmentation fault"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    /// A stray `println!` in a component shouldn't cost the user their build.
    #[test]
    fn a_stray_line_before_the_envelope_still_parses() {
        let e = Envelope::parse(&format!("warning: leaked to stdout\n{BUILD_OK}")).unwrap();
        assert!(e.is_ok());
        assert_eq!(e.field("dist_wasm"), Some("dist/app.wasm"));
    }

    #[test]
    fn an_unrecognized_status_still_yields_its_diagnostics() {
        let raw = r#"{"status":"cancelled","diagnostics":[{"level":"error","code":"X1","message":"stopped"}]}"#;
        let e = Envelope::parse(raw).unwrap();
        assert!(!e.is_ok());
        assert_eq!(e.errors().count(), 1);
    }

    #[test]
    fn severity_levels_all_round_trip() {
        for (json, level) in [
            ("error", Severity::Error),
            ("warning", Severity::Warning),
            ("info", Severity::Info),
            ("help", Severity::Help),
        ] {
            let raw = format!(
                r#"{{"status":"ok","diagnostics":[{{"level":"{json}","code":"C","message":"m"}}]}}"#
            );
            let e = Envelope::parse(&raw).unwrap();
            assert_eq!(e.diagnostics[0].level, level);
            assert_eq!(level.as_str(), json);
        }
    }
}
