//! Terminal rendering for dispatched components' output.
//!
//! Manager is the toolchain's single diagnostic renderer (PLAN.md §3). The
//! framework and compiler emit Platform 13 diagnostics as JSON; this module is
//! the only place that turns them into text a person reads, so a diagnostic
//! looks the same no matter which component produced it.
//!
//! Colors are opt-out via `NO_COLOR` (the de-facto standard) and are suppressed
//! automatically when stderr is not a terminal, so piped output stays clean.

use cln_dispatch::envelope::{Diagnostic, Envelope, Severity};

/// ANSI styling, resolved once per run.
#[derive(Copy, Clone, Debug)]
pub struct Style {
    enabled: bool,
}

impl Style {
    /// Enable color unless `NO_COLOR` is set or output is redirected.
    pub fn detect() -> Self {
        let enabled = std::env::var_os("NO_COLOR").is_none() && is_terminal();
        Self { enabled }
    }

    /// Styling disabled outright — used by tests, which assert on text.
    #[cfg(test)]
    pub fn plain() -> Self {
        Self { enabled: false }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn red(self, t: &str) -> String {
        self.paint("31;1", t)
    }
    fn yellow(self, t: &str) -> String {
        self.paint("33;1", t)
    }
    fn green(self, t: &str) -> String {
        self.paint("32;1", t)
    }
    fn cyan(self, t: &str) -> String {
        self.paint("36", t)
    }
    fn dim(self, t: &str) -> String {
        self.paint("2", t)
    }
}

#[cfg(unix)]
fn is_terminal() -> bool {
    // SAFETY: isatty(3) takes a file descriptor and only reads terminal state.
    unsafe { libc_isatty(2) == 1 }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
}

#[cfg(not(unix))]
fn is_terminal() -> bool {
    true
}

/// Render one diagnostic in the Platform 13 shape.
///
/// ```text
/// error[CFG005]: invalid UTF-8 in clean.toml
///   --> clean.toml:3:1
///   note: the file must be UTF-8
///   help: re-save the file as UTF-8
/// ```
pub fn render_diagnostic(d: &Diagnostic, style: Style) -> String {
    let label = match d.level {
        Severity::Error => style.red(&format!("error[{}]", d.code)),
        Severity::Warning => style.yellow(&format!("warning[{}]", d.code)),
        Severity::Info => style.cyan(&format!("info[{}]", d.code)),
        Severity::Help => style.cyan(&format!("help[{}]", d.code)),
    };

    let mut out = format!("{label}: {}", d.message);

    if let Some(span) = &d.primary_span {
        out.push('\n');
        out.push_str(&style.dim(&format!(
            "  --> {}:{}:{}",
            span.file, span.start.line, span.start.column
        )));
        if let Some(l) = &d.primary_label {
            out.push_str(&format!("\n   {l}"));
        }
    }

    for a in &d.secondary {
        out.push('\n');
        out.push_str(&style.dim(&format!(
            "  --> {}:{}:{}: {}",
            a.span.file, a.span.start.line, a.span.start.column, a.label
        )));
    }
    for n in &d.notes {
        out.push_str(&format!("\n  {} {n}", style.cyan("note:")));
    }
    for h in &d.helps {
        out.push_str(&format!("\n  {} {h}", style.cyan("help:")));
    }
    if let Some(url) = &d.doc_url {
        out.push_str(&format!("\n  {} {url}", style.dim("see:")));
    }
    out
}

/// Just the one-line outcome, without re-printing diagnostics.
///
/// This is the default for dispatched verbs: `clean-framework` already wrote
/// its diagnostics to stderr, so manager adds the summary line and nothing
/// else. Use [`render_envelope`] when manager should own the full rendering.
pub fn render_summary(env: &Envelope, style: Style) -> String {
    outcome_line(env, style).unwrap_or_default()
}

/// Render the whole envelope: diagnostics first, then a one-line outcome.
///
/// The component already printed its own progress to stderr; this is the
/// summary that follows it.
pub fn render_envelope(env: &Envelope, style: Style) -> String {
    let mut lines: Vec<String> = env
        .diagnostics
        .iter()
        .map(|d| render_diagnostic(d, style))
        .collect();

    if let Some(summary) = outcome_line(env, style) {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(summary);
    }
    lines.join("\n")
}

/// The single line describing what the component produced.
fn outcome_line(env: &Envelope, style: Style) -> Option<String> {
    if !env.is_ok() {
        let n = env.errors().count();
        return Some(match n {
            0 => style.red("build failed").to_string(),
            1 => format!("{} (1 error)", style.red("build failed")),
            n => format!("{} ({n} errors)", style.red("build failed")),
        });
    }

    // Success: name the artifact and its digest. `package` reports a package,
    // `build` a wasm module; both go through the same shape.
    let (artifact, digest) = if let Some(p) = env.field("package") {
        (p, env.field("package_sha256"))
    } else if let Some(w) = env.field("dist_wasm") {
        (w, env.field("wasm_sha256"))
    } else {
        return Some(format!("{} ok", style.green("✓")));
    };

    let mut line = format!("{} {artifact}", style.green("✓"));
    if let Some(d) = digest {
        // A short digest is enough to eyeball whether two builds match.
        let short: String = d.chars().take(12).collect();
        line.push_str(&style.dim(&format!("  sha256 {short}")));
    }
    if let Some(v) = &env.framework_version {
        line.push_str(&style.dim(&format!("  framework {v}")));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Envelope {
        Envelope::parse(raw).unwrap()
    }

    #[test]
    fn renders_a_successful_build_with_artifact_and_digest() {
        let env = parse(
            r#"{"status":"ok","dist_wasm":"dist/app.wasm","wasm_sha256":"f00dcafebabe99","framework_version":"0.1.1"}"#,
        );
        let out = render_envelope(&env, Style::plain());
        assert!(out.contains("dist/app.wasm"));
        assert!(out.contains("sha256 f00dcafebabe"), "digest is shortened");
        assert!(out.contains("framework 0.1.1"));
    }

    #[test]
    fn renders_a_package_outcome() {
        let env = parse(
            r#"{"status":"ok","package":"dist/app.clapp","package_sha256":"abc123","kind":"clapp"}"#,
        );
        let out = render_envelope(&env, Style::plain());
        assert!(out.contains("dist/app.clapp"));
    }

    #[test]
    fn renders_a_diagnostic_with_span_note_and_help() {
        let env = parse(
            r#"{"status":"error","diagnostics":[{"level":"error","code":"CFG005","message":"invalid UTF-8","notes":["must be UTF-8"],"helps":["re-save it"],"primary_span":{"file":"clean.toml","start":{"line":3,"column":7},"end":{"line":3,"column":9}}}]}"#,
        );
        let out = render_envelope(&env, Style::plain());
        assert!(out.contains("error[CFG005]: invalid UTF-8"));
        assert!(out.contains("--> clean.toml:3:7"));
        assert!(out.contains("note: must be UTF-8"));
        assert!(out.contains("help: re-save it"));
        assert!(out.contains("build failed (1 error)"));
    }

    #[test]
    fn counts_multiple_errors() {
        let env = parse(
            r#"{"status":"error","diagnostics":[{"level":"error","code":"A","message":"one"},{"level":"error","code":"B","message":"two"}]}"#,
        );
        assert!(render_envelope(&env, Style::plain()).contains("build failed (2 errors)"));
    }

    #[test]
    fn warnings_do_not_count_as_errors() {
        let env = parse(
            r#"{"status":"ok","dist_wasm":"a.wasm","diagnostics":[{"level":"warning","code":"W1","message":"unused"}]}"#,
        );
        let out = render_envelope(&env, Style::plain());
        assert!(out.contains("warning[W1]"));
        assert!(!out.contains("build failed"));
    }

    #[test]
    fn plain_style_emits_no_ansi_escapes() {
        let env = parse(
            r#"{"status":"error","diagnostics":[{"level":"error","code":"E","message":"m"}]}"#,
        );
        assert!(!render_envelope(&env, Style::plain()).contains('\x1b'));
    }

    #[test]
    fn color_style_emits_ansi_escapes() {
        let env = parse(
            r#"{"status":"error","diagnostics":[{"level":"error","code":"E","message":"m"}]}"#,
        );
        let styled = render_envelope(&env, Style { enabled: true });
        assert!(styled.contains('\x1b'));
    }
}
