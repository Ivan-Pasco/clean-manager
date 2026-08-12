# Clean Manager — Implementation Plan

Build plan for `clean-manager`, the developer-facing binary (`cln`) that ties the Clean Language toolchain together. Derived from `foundation/02 components/manager/00-manager.md` (the spec), `foundation/01 governance/01-architecture-boundaries.md §2.3` (what manager is NOT allowed to do), and `foundation/03 platform/07,14,15` for the schemas manager reads and writes.

Manager's job, one sentence: **be the only binary a developer installs — parse `cln <verb>`, resolve versions, install/switch/pin toolchain artifacts, fetch libraries, and dispatch to the right component binary.** Manager never compiles, never builds, never runs wasm. It hands off.

Because manager ships FIRST in the build order (Manager → Framework → Server), it has to work end-to-end against components that don't exist yet. The plan treats "hello-world install path" — install the compiler binary, pin it, and stub-dispatch a `cln build` that succeeds — as the M0 milestone.

---

## 1. Language and toolchain

**Choice: Rust.**

Rationale:

1. **Manager is on every developer's `$PATH`.** Single-binary distribution, sub-100ms startup, and zero-dependency install matter more here than anywhere else. Rust delivers a static binary; Node/Python require a runtime; Go is viable but the ecosystem for the specific things manager does (TOML round-trip, symlinks, self-update, per-OS file-association APIs) is stronger in Rust.
2. **Shared crates with framework and compiler.** `clean.toml` schema, `RequestDocument` type (Platform 14 §14.1.1), diagnostic format (Platform 13). Wire compatibility comes for free when both sides deserialize the same Rust types.
3. **`~/.cln/` is a filesystem-heavy workload.** Symlinks under `~/.cln/active/`, version-folder resolution, lockfile writing, SHA-256 verification on every fetch. Rust's `std::fs` + `std::os::unix::fs::symlink` handles this without a runtime surprise; the corresponding Node story is `fs-extra` plus platform-specific branches.
4. **Self-update.** Manager must atomically replace its own binary on `cln self-update`. Rust has a mature pattern for this (`self_update` crate, or a small custom implementation using `rename(2)` on Unix, `MoveFileEx` on Windows). Straightforward but not something you want to invent in a scripting language.

**Reference-stack picks (subject to an ADR before we commit):**

- Argv: `clap` v4 with derive. The flat verb surface (Manager §00.3) fits `clap` cleanly.
- TOML: `toml_edit` for round-trip fidelity when we write `clean.toml` (adding a `[dependencies]` entry via `cln add` must preserve comments and formatting), plus plain `toml` for read-only parsing of `.cln/lock.toml`.
- HTTP: `ureq` (blocking, small dependency tree). Async isn't needed — manager fetches one artifact at a time and the wait is dominated by the network anyway.
- SHA-256: `sha2` for artifact verification.
- ZIP / tar.gz: `zip` + `flate2` / `tar` — release artifacts arrive as one or the other depending on the OS.
- OCI registry client (deferred to M3): probably `oci-distribution` or a direct implementation against the registry HTTP API.
- Per-OS file-association APIs (§00.12): direct `windows-sys` for the registry keys on Windows, shell out to `xdg-mime` on Linux, `plutil` + `LSRegisterURL` on macOS.

---

## 2. Crate / module layout

Single Cargo workspace. One shipping binary (`cln`). Small crates so pieces are independently testable and framework/compiler can depend on shared types without pulling in the whole manager.

```
clean-manager/
├── Cargo.toml                          # workspace root
├── PLAN.md                             # this file
│
├── crates/
│   ├── cln-shared/                     # types shared with framework and compiler
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── clean_toml.rs           # clean.toml schema (foundation/schema/clean.toml.md)
│   │       ├── lock.rs                 # .cln/lock.toml schema
│   │       ├── request_document.rs     # Platform 14 §14.1.1 shape (framework produces, compiler consumes)
│   │       ├── build_manifest.rs       # Platform 14 §14.8 shape (compiler produces)
│   │       ├── diagnostic.rs           # Platform 13 shape (rendered by manager)
│   │       └── version.rs              # semver constraints + resolution primitives
│   │
│   ├── cln-layout/                     # ~/.cln/ on-disk layout (Manager §00.2)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── paths.rs                # every path under ~/.cln/ as a typed accessor
│   │       ├── versions.rs             # ~/.cln/versions/{compiler,framework,runtime}/<version>/
│   │       ├── active.rs               # ~/.cln/active/ symlink management
│   │       ├── plugins.rs              # ~/.cln/plugins/<name>/<version>/
│   │       ├── caches.rs               # ~/.cln/cache/, ~/.cln/wit-cache/, ~/.cln/build-cache/
│   │                                   #   NOT ~/.cln/host-wit/ — see "Host contract seeding" below
│   │       └── config.rs               # ~/.cln/config.toml (manager's own state)
│   │
│   ├── cln-project/                    # per-project .cln/ under user's projects
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── pins.rs                 # .cln/version, .cln/frame-version, .cln/runtime-version
│   │       ├── lockfile.rs             # .cln/lock.toml read + write
│   │       └── discover.rs             # find project root by walking up from cwd looking for clean.toml
│   │
│   ├── cln-resolver/                   # dependency resolution (Manager §00.5)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── source.rs               # SourceKind = Registry | Git | Path
│   │       ├── path.rs                 # { path = "..." } deps
│   │       ├── git.rs                  # { git = "...", tag = "..." } deps — via `git` subprocess
│   │       ├── registry.rs             # OCI registry client (deferred to M3; stub in M0/M1)
│   │       ├── solver.rs               # semver SAT solver — small hand-rolled (or pubgrub crate)
│   │       └── verify.rs               # checksum verification against lockfile
│   │
│   ├── cln-install/                    # toolchain artifact installation (Manager §00.3.3)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── channels.rs             # release channel + latest-version resolution
│   │       ├── download.rs             # fetch + verify + extract into ~/.cln/versions/<kind>/<version>/
│   │       ├── activate.rs             # switch ~/.cln/active/<kind> symlink
│   │       └── uninstall.rs            # remove version, prevent uninstalling the active one
│   │
│   ├── cln-dispatch/                   # verb → component-binary dispatch (Manager §00.4)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── table.rs                # the routing table (which verb → which component)
│   │       ├── framework.rs            # spawn ~/.cln/versions/framework/<version>/clean-framework <verb...>
│   │       ├── runtime.rs              # spawn ~/.cln/versions/runtime/<version>/clean-runtime for `cln run`
│   │       └── stream.rs               # stream stdout/stderr + exit code back to user's terminal
│   │
│   ├── cln-run/                        # `cln run <path>` (Manager §00.13)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── artifact.rs             # detect .clapp | .serve | .wasm | project dir
│   │       ├── manifest.rs             # read manifest.toml from .clapp/.serve archives
│   │       ├── extract.rs              # unpack .clapp/.serve into ~/.cln/cache/run/<sha>/
│   │       └── invoke.rs               # pick runtime version + world, invoke runtime binary
│   │
│   ├── cln-register/                   # OS file associations (Manager §00.12)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── state.rs                # ~/.cln/registrations/state.toml
│   │       ├── macos.rs                # LSRegisterURL + minimal .app bundle
│   │       ├── windows.rs              # HKCU\Software\Classes registry keys
│   │       └── linux.rs                # xdg-mime + .desktop file
│   │
│   ├── cln-doctor/                     # `cln doctor` diagnostics (Manager §00.3.7)
│   │   └── src/
│   │       ├── lib.rs
│   │       └── checks.rs               # PATH, symlinks, version-matrix, cache health, registry reachability
│   │
│   ├── cln-telemetry/                  # adoption heartbeat (Manager §00.10)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── heartbeat.rs            # minimal opaque UUID + version tuple
│   │       └── consent.rs              # on/off/status
│   │
│   ├── cln-self-update/                # `cln self-update` (Manager §00.3.3)
│   │   └── src/
│   │       ├── lib.rs
│   │       └── replace.rs              # atomic binary replacement per OS
│   │
│   ├── cln-shell/                      # first-run shell PATH setup (Manager §00.6)
│   │   └── src/
│   │       ├── lib.rs
│   │       └── rc.rs                   # detect bash/zsh/fish/pwsh, inject with guarded marker
│   │
│   └── cln-cli/                        # main binary — the `cln` you type
│       └── src/
│           ├── main.rs
│           ├── verbs/                  # one file per verb group; each is thin argv → lib call
│           │   ├── project.rs          # new, templates, check, build, package, run, dev, ship, test
│           │   ├── deps.rs             # add, remove, fetch, lock, list
│           │   ├── toolchain.rs        # install, use, pin, sync, uninstall, cleanup, available
│           │   ├── db.rs               # db migrate <verb>
│           │   ├── api.rs              # api spec, api sdk
│           │   ├── mcp.rs              # mcp, mcp install
│           │   ├── diag.rs             # doctor, explain, repro, report, fixes, telemetry
│           │   ├── library.rs          # library create, library build, library test, library publish
│           │   └── os.rs               # register, unregister, register --status
│           └── output.rs               # terminal rendering: colors, diagnostic pretty-print
│
├── testing/
│   ├── fake-framework/                 # binary that speaks the framework CLI contract with canned responses
│   ├── fake-runtime/                   # binary that pretends to run wasm — echoes args + world
│   ├── fixtures/                       # sample ~/.cln/ trees, sample projects, sample lockfiles
│   └── golden/                         # golden dispatch traces, golden lockfile shapes
│
└── docs/
    └── (empty initially; ADRs land here as we lock decisions)
```

**Why this shape:**

- `cln-shared` is the crate framework and compiler pull in. Only types, no logic. That's what keeps three components on the same wire format without a shared build.
- `cln-layout` is a boring typed accessor over `~/.cln/`. Everything else that needs a path calls `cln_layout::versions::compiler(version)` rather than string-formatting. Refactoring the layout later touches one crate.
- Every verb group is a separate module under `cln-cli/src/verbs/` so the argv surface stays inspectable and easy to test. The verb function is 5–20 lines: parse flags, call the library, format output.
- `cln-dispatch` is the whole subprocess story. Every framework-dispatched verb (`cln build`, `cln dev`, `cln new`, `cln db migrate`) goes through it. If we later decide to link framework in-process instead of subprocess, we swap one crate.
- `cln-run` and `cln-dispatch` are separate because `cln run` dispatches to the *runtime*, not the framework — different resolution rules (per-artifact runtime pin, not per-project) and different invocation shape.

**Spec → module map (load-bearing rules):**

| Spec section | Module |
|---|---|
| MGR-01 (one front door), §00.4 (dispatch) | `cln-dispatch`, `cln-cli/verbs/*` |
| MGR-02 (bounded on-disk footprint), §00.2 | `cln-layout` (only crate allowed to write outside cwd) |
| MGR-03 (file associations), §00.12 | `cln-register` |
| MGR-04 (runtime as managed artifact), §00.13 | `cln-install`, `cln-run` |
| MGR-05 (`.clapp` / `.serve`), §00.14 | `cln-run::manifest`, `cln-run::extract` (read side); framework owns write side |
| §00.5 resolution + fetching | `cln-resolver`, `cln-install::download` |
| §00.6 install + shell integration | `cln-shell` |
| §00.7 version matrix | `cln-install::channels`, `cln-doctor::checks` |
| §00.8 framework interaction (`cln fetch --internal`) | `cln-dispatch::framework`, `cln-resolver` |
| §00.10 telemetry | `cln-telemetry` |
| Architecture Boundaries §2.3 (what manager MUST NOT do) | Enforced by module topology — no `cln-*` crate depends on any parser/codegen/wasm-runtime crate |

### Host contract seeding (`~/.cln/host-wit/`)

`cln install` writes the host contracts it ships into `~/.cln/host-wit/`, one
file per `<host>@<version>.wit`. Implemented in `cln-install::hostwit`; the path
accessor is `Layout::host_wit_dir()`.

**This is not `wit-cache/`.** Manager §00.2 lists both directories and they hold
different things: `wit-cache/` holds WIT *synthesized from library declarations*
and belongs to `cln-layout::caches`; `host-wit/` holds `host.wit` files
*published by hosts*, byte-for-byte. Do not merge them.

**Why seeded rather than fetched.** C-18 promises every command works offline. A
project's first `cln build` must validate the guest against the target host's
contract, and on a cold cache there is nothing to read. The contracts are
therefore embedded in the binary with `include_str!` and land on disk at install
time. Fetching at install would move the network round trip earlier without
removing it.

**Not a boundary violation.** Writing a text file the manager ships is version
management, not framework logic — nothing here parses `.cln`, reads project
folders, or generates code. Likewise, publishing `hosts/<host>/host.wit` for a
host *this repo owns* is a host declaring its own contract (HCV-02), not the
manager reimplementing framework behavior.

**No `clean-cli` contract yet.** HCV-06 makes a declared-but-unimplemented
interface a hard failure, and no CLI host implements `clean:host@0.1.0` today —
`clean-runtime` is a name in `ToolchainKind` and a repo reference with nothing
behind it. `hosts/clean-cli/host.wit` gets published, and a `clean-cli` entry
gets added to `CONTRACTS`, when that binary exists. Until then `wasm32-cli`
stays blocked.

The vendored contracts under `crates/cln-install/vendor/host-wit/` are copies of
files other repos own. `scripts/check_vendored_wit.sh` (run by CI) fails when
they drift from either their pinned hash or upstream at the pinned tag.

---

## 3. Public API shape

Manager is the top of the stack — no other component consumes it as a library. Everything is a CLI entry point.

**Primary surface: the `cln` binary.** Every user-facing operation is a subcommand parsed by `clap` in `cln-cli/src/main.rs`, delegating to one function in the appropriate `verbs/` module.

**Secondary surface: internal call-backs from framework.** The framework spawns `cln fetch --internal` (Manager §00.8) to trigger dependency resolution mid-build. That's not a public API — it's an unstable internal contract between manager and framework, both shipped together. Signature:

```
cln fetch --internal --project=<absolute-path> [--offline]
  → exit 0 on success, .cln/lock.toml updated
  → exit non-zero on failure, structured error on stderr
```

Framework parses stderr on failure. This shape lets us change it in coordinated releases without a stable-API burden.

**Verb entry-point signatures (in `cln-cli/verbs/`):**

Every verb function has the same shape — takes parsed args + a shared `Env` (cwd, `~/.cln/` root, offline flag, verbosity), returns `Result<ExitCode, ManagerError>`.

```rust
pub fn build(args: BuildArgs, env: &Env) -> Result<ExitCode, ManagerError>;
pub fn install(args: InstallArgs, env: &Env) -> Result<ExitCode, ManagerError>;
pub fn add(args: AddArgs, env: &Env) -> Result<ExitCode, ManagerError>;
// ...one per verb
```

**Diagnostic rendering.** When a dispatched component (framework, compiler) fails, it emits Platform 13 diagnostics on stdout in JSON. Manager parses and pretty-prints. This is why `cln-shared::diagnostic` is shared — both the emitter (compiler/framework) and the renderer (manager) use the same Rust type.

---

## 4. Build order

Every phase is a working system. No half-implementations.

**Phase 0 — Skeleton.** Cargo workspace per §2. `cln-shared` compiles with round-tripping tests for `clean.toml` and `.cln/lock.toml`. `cln --version` prints something. That's it.

**Phase 1 — On-disk layout + install.** *M0 milestone target.*

- `cln-layout`: create `~/.cln/`, `~/.cln/bin/`, `~/.cln/versions/{compiler,framework,runtime}/`, `~/.cln/active/`, `~/.cln/plugins/`, `~/.cln/cache/`, `~/.cln/config.toml`.
- `cln-install`: `cln install compiler <version>` downloads a canned artifact from a placeholder URL (or a local file for M0 tests), verifies SHA-256, extracts to `~/.cln/versions/compiler/<version>/`. `cln use compiler <version>` flips the `~/.cln/active/compiler` symlink. Same story for `framework` and `runtime`.
- `cln install latest` fetches the release-channel manifest and picks newest stable. For M0 the "channel" can be a static JSON file we host locally.
- `cln uninstall`, `cln available`, `cln list`.
- **The M0 milestone is:** run `cln install compiler <v> && cln install framework <v> && cln install runtime <v>` from a fresh `~/.cln/` and end up with three working symlinks. No project, no build yet.

**Phase 2 — Project pins + dispatch.**

- `cln-project`: locate project root, read/write `.cln/version`, `.cln/frame-version`, `.cln/runtime-version`.
- `cln pin <version>` writes all three; `cln sync` reads them and calls `cln install` for anything missing.
- `cln-dispatch`: resolve which framework binary to launch (per-project pin overrides global active), spawn it with the argv the user typed (minus the `cln` prefix), stream stdout/stderr, propagate exit code.
- `cln build` and `cln dev` now dispatch to framework. Framework doesn't have to work yet — testing uses `fake-framework` binary that echoes args and exits 0.

**Phase 3 — Dependency resolution (path + git only).**

- `cln-resolver`: `SourceKind::Path` (read `../shared/library.toml`), `SourceKind::Git` (shell out to `git clone --depth=1 --branch <tag>` into `~/.cln/cache/git/<sha>/`).
- `cln-project::lockfile`: write `.cln/lock.toml` with resolved versions + checksums.
- `cln add <name>`, `cln remove <name>`, `cln fetch`, `cln lock`.
- `cln fetch --internal --project=<path>` — the framework callback shape (§00.8).
- Registry (`SourceKind::Registry`) is deferred to M3 per your earlier decision.

**Phase 4 — `cln run`.**

- Detect artifact type: project directory → dispatch `cln build` then run `dist/app.wasm`; `.clapp` / `.serve` → extract to `~/.cln/cache/run/<sha>/`, read `manifest.toml`, resolve runtime version, invoke runtime binary; bare `.wasm` → invoke with default world.
- Runtime resolution order per §00.13: artifact manifest exact-pin → project `.cln/runtime-version` → global `~/.cln/active/runtime`.

**Phase 5 — Scaffolding + self-update + shell.**

- `cln new <template> <path>` — dispatches to framework which owns templates.
- `cln self-update` — download new manager binary, atomic replace.
- `cln-shell`: first-run PATH injection into `~/.zshrc` etc. with a guarded marker comment.
- `cln doctor` — PATH check, symlinks intact, active versions installed, cache health.

**Phase 6 — File associations, telemetry, MCP.**

- `cln register`, `cln unregister`, `cln register --status` (§00.12).
- `cln telemetry on|off|status` (§00.10).
- `cln mcp install` — write MCP client config for Claude Code, VS Code, Cursor.
- `cln explain <CODE>` — dispatches to framework (which invokes compiler API per §00.4 dispatch table).

**Phase 7 — Everything else.** `cln repro`, `cln report`, `cln fixes`, `cln db migrate <verb>` (dispatches to framework), `cln api spec/sdk` (dispatches to framework).

---

## 5. Testing strategy

Same three-layer pattern as the framework plan.

**Layer A — unit tests.** Per module. `cln-layout::paths` gets a tempdir; asserts every documented path is producible. `cln-resolver::solver` gets a synthetic dep graph; asserts version selection. `cln-project::lockfile` round-trips a canned lockfile.

**Layer B — orchestration tests with fakes.** `testing/fake-framework` is a small binary that speaks the framework CLI contract (accepts `build`, `dev`, `new`, etc. as argv), writes a canned response to stdout, exits with a canned code. Install it into a test `~/.cln/versions/framework/<v>/` and run real manager verbs against it. Tests dispatch, streaming, error propagation, and pin-resolution without needing the real framework. Same for `fake-runtime` for `cln run` tests.

**Layer C — integration tests with real components.** In CI (not per-commit): install a real pinned framework version + real pinned compiler + real pinned runtime, run `cln new hello && cd hello && cln build && cln run` end-to-end.

**Determinism.** `.cln/lock.toml` writes are byte-deterministic. `~/.cln/config.toml` writes are byte-deterministic. Same test applies as in framework.

**Cross-platform.** Manager is where per-OS surface actually diverges — file associations, shell rc files, symlink vs junction. Every module with a per-OS branch needs a test matrix (macOS, Linux, Windows). Windows in particular tends to surprise.

---

## 6. Milestones

**M0 — Toolchain install works.** *~2 weeks from starting.*

Deliverables:
- Workspace + `cln-shared` + `cln-layout` + `cln-install` + minimal `cln-cli`.
- `cln install <kind> <version>` from a local test artifact.
- `cln use <kind> <version>` flips symlinks.
- `cln --version`, `cln list`, `cln uninstall`.
- Layer A tests for `cln-layout` and `cln-install`.

Explicit non-goals: dependency resolution, dispatch, `cln run`, MCP, file associations.

**M1 — Dispatch + pins + resolution.** *~4 weeks after M0.*

Deliverables:
- `cln-project` (pins, project root discovery).
- `cln-dispatch` (framework subprocess).
- `cln pin`, `cln sync`, `cln build` (routes through `fake-framework` in tests, real framework once M0-framework is done).
- `cln-resolver` for path + git deps.
- `cln add`, `cln remove`, `cln fetch`, `cln lock`.
- `cln fetch --internal` callback shape working.
- Layer B tests using `fake-framework`.

Explicit non-goals: `cln run`, packaging, file associations, MCP.

**M2 — `cln run` + scaffolding + doctor.** *~4 weeks after M1.*

Deliverables:
- `cln-run` — full `.clapp` / `.serve` / project-dir / raw-wasm dispatch.
- `cln new <template>`.
- `cln self-update`.
- `cln-shell` first-run PATH injection.
- `cln doctor`.
- `cln explain <CODE>`.

**M3 — Registry + file associations + polish.** *~4 weeks after M2.*

Deliverables:
- OCI registry client (Manager §00.11.1).
- Community library signing verification (§00.11.2).
- `cln register`, `cln unregister` — all three OSes.
- `cln telemetry`.
- `cln mcp install`.
- `cln repro`, `cln report`, `cln fixes` (or stubs pointing at the error-reporting flow).

**M4+ — Workspaces, shared cache, multi-channel.** All the deferred refinements from §00.11.

---

## 7. Open questions

Answers proposed; confirm before Phase 2.

1. **Release channel format.** `cln install latest` needs a machine-readable manifest listing published versions per kind. Proposal: `https://install.cleanlanguage.dev/channels/stable.json` with shape `{ "compiler": [{"version": "1.0.0", "url": "...", "sha256": "..."}, ...], "framework": [...], "runtime": [...] }`. For M0 use a local file. Confirm domain + shape.

2. **Version matrix location.** §00.7 says the version matrix is "a machine-readable file published by the Clean team alongside each compiler release." Proposal: sits next to the compiler artifact at `https://install.cleanlanguage.dev/matrix/<compiler-version>.json`, shape `{ "framework_compatible": ["^2.1"], "runtime_compatible": ["^1.0"], "libraries_matrix": {...} }`. Confirm.

3. **Where does manager get its own version for `cln --version`?** Same answer as framework: `env!("CARGO_PKG_VERSION")`. Confirm no separate manifest.

4. **Component binary naming.** Spec says `clean-compiler`, `clean-framework`, `clean-runtime`. Proposal: installer extracts them under those names into `~/.cln/versions/<kind>/<version>/`, no rename. Confirm no `.exe` handling surprises on Windows (yes, needs `.exe` suffix on Win).

5. **Framework CLI contract.** Manager's dispatch depends on framework's argv being stable. Proposal: framework declares its stable CLI contract in its repo's `docs/CLI-CONTRACT.md`, versioned; a framework CLI change without a coordinated manager update is a bug. Confirm.

6. **Self-update trust model.** `cln self-update` replaces the binary a user is currently running with a newly-downloaded one. How is the download trusted — signed binary + public key baked into `cln`? Signed manifest + checksum? Proposal: signed manifest (Ed25519, public key baked into `cln` at build time) listing per-OS binary URLs + SHA-256; verify signature, then SHA-256, then atomic replace. Confirm.

7. **Concurrent `cln` invocations.** Two shells running `cln fetch` in the same project at the same time. Manager should either block one on a lockfile or fail loudly. Proposal: advisory file lock at `.cln/.lock`; second invocation waits up to 30s then errors. Confirm.

8. **`--offline` inheritance.** `cln --offline build` should propagate offline mode to framework which should propagate to any dep-fetching callback. Proposal: `--offline` sets `CLN_OFFLINE=1` environment variable that every dispatched process reads. Confirm.

9. **What happens when the pinned compiler version isn't installed?** `.cln/version = "1.0.0"` but `~/.cln/versions/compiler/1.0.0/` doesn't exist. Proposal: `cln build` fails with a diagnostic pointing at `cln sync` (which will install it). Do NOT auto-install without user confirmation — silent installs during a build surprise people. Confirm.

---

## Metadata

- **Author:** manager session (Ivo Pasco, 2026-08-09)
- **Status:** Draft for review
- **Owned decisions locked before writing:** Rust; single-binary; subprocess dispatch to framework and runtime; path+git deps only in M0/M1 (registry deferred to M3); shared crate (`cln-shared`) for wire types.
- **Depends on (both ways):** framework's CLI contract (dispatch), compiler's CLI contract (only reached via framework, but manager's `cln explain` proxies to it). Nothing links against manager as a library — it's the top of the stack.
- **Next step after review:** convert accepted plan into ADR-0001 for manager, scaffold the Cargo workspace, land Phase 0.
