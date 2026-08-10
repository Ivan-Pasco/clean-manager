use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The three toolchain artifacts manager installs, pins, and switches.
///
/// Manager §00.3.3 defines these; every `cln install / use / pin / uninstall /
/// available` verb accepts one of these as an optional kind argument.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolchainKind {
    Compiler,
    Framework,
    Runtime,
}

impl ToolchainKind {
    pub const ALL: [ToolchainKind; 3] = [
        ToolchainKind::Compiler,
        ToolchainKind::Framework,
        ToolchainKind::Runtime,
    ];

    /// The lowercase name used everywhere on disk and in argv
    /// (`~/.cln/versions/<name>/`, `cln install <name> <version>`).
    pub fn as_str(self) -> &'static str {
        match self {
            ToolchainKind::Compiler => "compiler",
            ToolchainKind::Framework => "framework",
            ToolchainKind::Runtime => "runtime",
        }
    }

    /// The component binary this kind installs. Manager §00.2 pins these names.
    pub fn binary_name(self) -> &'static str {
        match self {
            ToolchainKind::Compiler => "clean-compiler",
            ToolchainKind::Framework => "clean-framework",
            ToolchainKind::Runtime => "clean-runtime",
        }
    }
}

impl fmt::Display for ToolchainKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown toolchain kind '{0}' (expected: compiler, framework, runtime)")]
pub struct ParseKindError(String);

impl FromStr for ToolchainKind {
    type Err = ParseKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "compiler" => Ok(ToolchainKind::Compiler),
            "framework" => Ok(ToolchainKind::Framework),
            "runtime" => Ok(ToolchainKind::Runtime),
            other => Err(ParseKindError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_as_str_and_from_str() {
        for k in ToolchainKind::ALL {
            assert_eq!(k, k.as_str().parse().unwrap());
        }
    }

    #[test]
    fn binary_names_match_spec() {
        assert_eq!(ToolchainKind::Compiler.binary_name(), "clean-compiler");
        assert_eq!(ToolchainKind::Framework.binary_name(), "clean-framework");
        assert_eq!(ToolchainKind::Runtime.binary_name(), "clean-runtime");
    }

    #[test]
    fn unknown_kind_errors() {
        assert!("nope".parse::<ToolchainKind>().is_err());
    }

    #[test]
    fn serde_lowercase() {
        let json = serde_json::to_string(&ToolchainKind::Compiler).unwrap();
        assert_eq!(json, "\"compiler\"");
        let back: ToolchainKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ToolchainKind::Compiler);
    }
}
