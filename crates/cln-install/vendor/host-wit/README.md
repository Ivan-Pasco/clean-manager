# Vendored host contracts

These `.wit` files are **copies of files other repositories own**. They are
embedded into the `cln` binary with `include_str!` and seeded into
`~/.cln/host-wit/` at install time, so a project's first `cln build` works
offline (C-18) instead of needing a network round trip.

## Do not hand-edit anything in this directory

The framework hashes the contract it reads and pins that hash into
`.cln/lock.toml` ([BVER-03]). Any edit — a reflowed comment, a changed line
ending, an added trailing newline — changes the hash and breaks every project
pinned against it. These files are byte-for-byte copies and must stay that way.

To update a contract, re-copy it from upstream at a new version and add a new
entry to `CONTRACTS` in `../../src/hostwit.rs`. Do not mutate an existing file
in place: a published `<host>@<version>` is immutable.

## Provenance

| File | Upstream repo | Upstream path | Version | SHA-256 |
|---|---|---|---|---|
| `clean-server@0.7.0.wit` | `Ivan-Pasco/clean-server` | `host.wit` (repo root, per [HCV-02]) | `v0.7.0` (commit `54ca10d`) | `c4aaba83494e63577cb798e1483ce6604c6e55660010c5d0ced3be0d2a6963de` |

The WIT package inside each file is `clean:host@0.1.0` — one package for every
host contract, per [CMOD-01]. The *file* version above is the **host's** release
version, which is what names the cache entry (`<host>@<version>.wit`); it is not
the WIT package version and the two move independently.

## Drift

`scripts/check_vendored_wit.sh` re-derives each file's SHA-256 and compares it
against the pinned constant in `src/hostwit.rs`, and — when the upstream repo is
checked out as a sibling — against upstream at the pinned tag. CI runs it on
every commit. It fails loudly rather than silently shipping a stale contract.

[BVER-03]: https://github.com/Ivan-Pasco/clean-language-foundation/blob/main/03%20platform/08-bridge-versioning.md
[HCV-02]: https://github.com/Ivan-Pasco/clean-language-foundation/blob/main/03%20platform/16-host-contract-validation.md
[CMOD-01]: https://github.com/Ivan-Pasco/clean-language-foundation/blob/main/03%20platform/15-component-model-architecture.md
