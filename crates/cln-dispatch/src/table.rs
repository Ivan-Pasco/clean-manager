//! The routing table: which verb is implemented by which component
//! (Manager §00.4).
//!
//! Manager owns the *names* users type; components own the behavior. This table
//! is the seam. A verb that appears here is one manager will forward rather
//! than handle itself — manager's own verbs (`install`, `use`, `list`, …) are
//! deliberately absent.
//!
//! Adding a verb once the owning component ships it is a line in [`ROUTES`],
//! not new code.

use cln_shared::ToolchainKind;

/// One verb's routing entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Route {
    /// The verb as the user types it after `cln`.
    pub verb: &'static str,
    /// The component that implements it.
    pub component: ToolchainKind,
    /// The subcommand name to pass to the component binary.
    ///
    /// Usually identical to `verb`; kept separate because manager's user-facing
    /// name and a component's internal subcommand are allowed to diverge.
    pub forwards_as: &'static str,
}

/// Every verb manager forwards to another component.
///
/// Only verbs the installed components actually implement belong here.
/// `clean-framework` 0.1.1 ships exactly `build` and `package`; `dev` and `new`
/// are specified in PLAN.md Phase 2/5 but have no counterpart in the binary
/// yet, so routing them would produce a bare clap exit-2 with no explanation of
/// why. They get added here when the framework ships them.
pub const ROUTES: &[Route] = &[
    Route {
        verb: "build",
        component: ToolchainKind::Framework,
        forwards_as: "build",
    },
    Route {
        verb: "package",
        component: ToolchainKind::Framework,
        forwards_as: "package",
    },
];

/// The route for a verb, or `None` if manager does not forward it.
pub fn route(verb: &str) -> Option<Route> {
    ROUTES.iter().copied().find(|r| r.verb == verb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_package_route_to_the_framework() {
        for verb in ["build", "package"] {
            let r = route(verb).unwrap_or_else(|| panic!("{verb} should be routed"));
            assert_eq!(r.component, ToolchainKind::Framework);
            assert_eq!(r.forwards_as, verb);
        }
    }

    #[test]
    fn managers_own_verbs_are_not_dispatched() {
        for verb in ["install", "use", "uninstall", "list", "available"] {
            assert!(route(verb).is_none(), "{verb} is manager's own");
        }
    }

    #[test]
    fn unknown_verbs_do_not_route() {
        assert!(route("teleport").is_none());
    }

    #[test]
    fn verbs_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for r in ROUTES {
            assert!(seen.insert(r.verb), "duplicate route for {}", r.verb);
        }
    }
}
