#!/usr/bin/env bash
#
# Vendored host-contract drift check.
#
# The .wit files under crates/cln-install/vendor/host-wit/ are copies of files
# other repositories own. They are embedded into `cln` and seeded into
# ~/.cln/host-wit/, where the framework hashes them and pins the hash into
# project lock files (BVER-03). A silent edit here breaks every pinned project,
# so this fails loudly instead.
#
# Two checks:
#   1. Local pin  — each vendored file hashes to the constant in src/hostwit.rs.
#      Always runs.
#   2. Upstream   — each vendored file is byte-identical to the upstream repo at
#      the pinned tag. Runs when the upstream checkout is present as a sibling,
#      or when CLEAN_SERVER_DIR points at one; skipped with a notice otherwise
#      (a skip is reported, never silent).
#
# Usage: scripts/check_vendored_wit.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
vendor_dir="$repo_root/crates/cln-install/vendor/host-wit"
manifest="$repo_root/crates/cln-install/src/hostwit.rs"

fail=0
note() { printf '%s\n' "$*"; }
bad() { printf 'FAIL: %s\n' "$*" >&2; fail=1; }

sha_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

# --- 1. every vendored file matches its pinned constant --------------------

shopt -s nullglob
vendored=("$vendor_dir"/*.wit)
shopt -u nullglob

if [ ${#vendored[@]} -eq 0 ]; then
    bad "no vendored .wit files found in $vendor_dir"
    exit 1
fi

for file in "${vendored[@]}"; do
    base="$(basename "$file")"
    actual="$(sha_of "$file")"
    if grep -qF "$actual" "$manifest"; then
        note "ok    $base pinned at $actual"
    else
        bad "$base hashes to $actual, which does not appear in $(basename "$manifest") — the vendored file was edited without updating CONTRACTS"
    fi
done

# --- 2. every vendored file matches upstream at its pinned tag -------------

check_upstream() {
    local host="$1" version="$2" tag="$3" upstream_rel="$4" dir="$5"
    local vendored_file="$vendor_dir/$host@$version.wit"

    if [ ! -d "$dir/.git" ]; then
        note "skip  $host@$version upstream comparison — no checkout at $dir (set CLEAN_SERVER_DIR to enable)"
        return
    fi

    # A tag that does not exist upstream is a *pending publish*, not drift. A
    # contract can be vendored from a local tag before the owning repo pushes
    # it (clean-server v0.7.0 was in exactly this state when host-wit seeding
    # landed). Report it loudly and keep going: the local pin check above still
    # guards the bytes, and this leg starts enforcing the moment the tag
    # appears. What must never happen is the tag existing and disagreeing —
    # that falls through to the diff below and fails.
    if ! git -C "$dir" rev-parse -q --verify "refs/tags/$tag" >/dev/null 2>&1; then
        note "PENDING  $host@$version — $tag is not published in $dir yet; upstream comparison deferred"
        return
    fi

    local upstream
    if ! upstream="$(git -C "$dir" show "$tag:$upstream_rel" 2>/dev/null)"; then
        bad "$host@$version — $tag exists in $dir but has no $upstream_rel"
        return
    fi

    if printf '%s\n' "$upstream" | diff -q - "$vendored_file" >/dev/null 2>&1; then
        note "ok    $host@$version matches $dir@$tag:$upstream_rel"
    else
        bad "$host@$version has DRIFTED from $dir@$tag:$upstream_rel — re-vendor, or pin a new version"
    fi
}

check_upstream \
    "clean-server" "0.7.0" "v0.7.0" "host.wit" \
    "${CLEAN_SERVER_DIR:-$repo_root/../clean-server}"

exit "$fail"
