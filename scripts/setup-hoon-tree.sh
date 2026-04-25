#!/usr/bin/env bash
# Link external hoon trees that the NNS kernel needs to compile under
# hoonc. Idempotent — safe to re-run.
#
# Nockchain (`$NOCK_HOME`) provides:
#   - /common/v2/                 prover/verifier tables
#   - /common/v0-v1/              proof-version routing
#   - /common/stark/              STARK prover + verifier arms
#   - /common/nock-verifier.hoon  `verify` gate (Phase 1-redo bench)
#   - /dat/                       preprocessed constraints (softed)
#   - /jams/                      constraint jam artifacts
#
# Vesl (`$VESL_HOME`) provides:
#   - /lib/vesl-graft.hoon        settlement graft used by %settle-batch
#   - /lib/vesl-merkle.hoon       Merkle helpers used by %claim
#   - /lib/vesl-prover.hoon       STARK prover for arbitrary Nock (%prove-batch)
#   - /lib/vesl-stark-verifier.hoon   Level-2 STARK verifier (%verify-stark)
#   - /lib/vesl-verifier.hoon     softed-constraints wrapper over the above
#
# We used to vendor the vesl files as verbatim copies under
# `hoon/lib/vesl-*.hoon`. That drifted whenever vesl master moved and
# forced us to patch upstream fixes locally. Phase 2 promoted those
# files to symlinks so we track vesl master exactly. Phase 7.1
# re-vendored `vesl-stark-verifier.hoon` because it needs a local
# strict-hoonc patch — see the header on that file.
#
# Locally owned (untouched by this script):
#   - /common/wrapper.hoon
#   - /common/zeke.hoon           (a stub in hoon/common that /+-references zeke)
#   - /common/ztd/
#   - /lib/ (empty; everything lives upstream now)
#   - /app/
#   - /tests/
#
# Sibling checkouts are CANONICALLY named `../nockchain` and
# `../vesl-core` relative to this repo's root. The script hard-codes
# these paths so the committed symlinks resolve identically on every
# host — no machine-specific TOML reads, no env-var overrides.
#
# If you keep your checkouts elsewhere, symlink them at the expected
# location instead of configuring this script:
#
#   ln -s /path/to/nockchain  ../nockchain
#   ln -s /path/to/vesl-core  ../vesl-core
#
# Rust-side path deps in `Cargo.toml` assume the same canonical
# names, so pinning the script avoids surprise config drift.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOON_DIR="$REPO_ROOT/hoon"

NOCK_HOME="../nockchain"
VESL_HOME="../vesl-core"

# Resolve relative paths against repo root so symlinks are correct.
resolve_home() {
    local var_name="$1"
    local value="$2"
    local sentinel="$3"
    if [[ "$value" != /* ]]; then
        value="$(cd "$REPO_ROOT/$value" 2>/dev/null && pwd)" || {
            echo "Error: $var_name resolves to a missing directory: $value" >&2
            exit 1
        }
    fi
    if [[ ! -e "$value/$sentinel" ]]; then
        echo "Error: $value/$sentinel not found." >&2
        echo "Is $var_name pointing to the right repo root?" >&2
        exit 1
    fi
    echo "$value"
}

NOCK_HOME="$(resolve_home NOCK_HOME "$NOCK_HOME" hoon/common/v2)"
VESL_HOME="$(resolve_home VESL_HOME "$VESL_HOME" protocol/lib/vesl-graft.hoon)"

# Compute a relative path from `$2` (the symlink's parent directory) to
# `$1` (an absolute target path). Portable-ish: prefers GNU `realpath
# --relative-to` when available (Linux + Homebrew coreutils), falls
# back to Python 3 otherwise (macOS default). Both produce the same
# canonical relative path, which is what we commit to git so the repo
# is host-independent.
relpath() {
    local target="$1"
    local from_dir="$2"
    if command -v realpath >/dev/null 2>&1 && \
       realpath --help 2>&1 | grep -q relative-to; then
        realpath --relative-to="$from_dir" "$target"
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c "import os.path,sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))" \
            "$target" "$from_dir"
    else
        echo "Error: neither GNU realpath nor python3 available; can't compute relative path." >&2
        echo "       Install coreutils (brew install coreutils) or python3." >&2
        exit 1
    fi
}

link() {
    local target="$1"
    local dest="$2"
    local dest_dir
    dest_dir="$(dirname "$dest")"
    # Convert the absolute target to a repo-relative path so the
    # symlink is host-independent — can be committed and resolves on
    # any machine where the sibling checkout exists at the expected
    # location relative to the repo root.
    local rel_target
    rel_target="$(relpath "$target" "$dest_dir")"
    if [[ -L "$dest" ]]; then
        local current
        current="$(readlink "$dest")"
        if [[ "$current" == "$rel_target" ]]; then
            echo "  $(basename "$dest"): already linked"
            return
        fi
        rm "$dest"
    elif [[ -e "$dest" ]]; then
        echo "Error: $dest exists and is not a symlink. Refusing to clobber." >&2
        exit 1
    fi
    ln -s "$rel_target" "$dest"
    echo "  $(basename "$dest") -> $rel_target"
}

mkdir -p "$HOON_DIR/lib"

echo "Linking nockchain hoon subtrees from $NOCK_HOME ..."
link "$NOCK_HOME/hoon/common/v2"                 "$HOON_DIR/common/v2"
link "$NOCK_HOME/hoon/common/v0-v1"              "$HOON_DIR/common/v0-v1"
link "$NOCK_HOME/hoon/common/stark"              "$HOON_DIR/common/stark"
link "$NOCK_HOME/hoon/common/nock-verifier.hoon" "$HOON_DIR/common/nock-verifier.hoon"
link "$NOCK_HOME/hoon/common/zoon.hoon"          "$HOON_DIR/common/zoon.hoon"
link "$NOCK_HOME/hoon/dat"                       "$HOON_DIR/dat"
link "$NOCK_HOME/hoon/jams"                      "$HOON_DIR/jams"
# Phase 3 note: we deliberately do NOT symlink
# `/common/{tx-engine,tx-engine-0,tx-engine-1,pow,nock-prover,schedule,zose}`.
# When those are in scope alongside `/lib/vesl-prover.hoon` +
# `/lib/vesl-stark-verifier.hoon` (which already pull `stark/prover`
# via a different `=> stark-engine` path), hoonc ends up resolving
# the shared `/common/zeke` and `/common/ztd/*` trees twice and
# loops indefinitely. Symptom: `compiling /common/tx-engine-0.hoon`
# hangs for ~4 minutes before OOM-ing.
#
# `zoon.hoon` is safe — its only import is `/common/zeke`, no
# stark-engine cone. It's needed for `has:z-in` used by the Phase 3
# Level B `has-tx-in-page` predicate in `hoon/lib/nns-predicates.hoon`.
#
# Full tx-engine (for `raw-tx:v1` payment predicates) is still
# blocked; Phase 3 Level C will stage those via a narrow vendored
# `hoon/lib/tx-witness.hoon`. Do NOT add more tx-engine-cone
# symlinks here without first proving the compile stays bounded.

echo "Linking vesl hoon libs from $VESL_HOME ..."
link "$VESL_HOME/protocol/lib/vesl-graft.hoon"          "$HOON_DIR/lib/vesl-graft.hoon"
link "$VESL_HOME/protocol/lib/vesl-merkle.hoon"         "$HOON_DIR/lib/vesl-merkle.hoon"
link "$VESL_HOME/protocol/lib/vesl-prover.hoon"         "$HOON_DIR/lib/vesl-prover.hoon"
link "$VESL_HOME/protocol/lib/vesl-verifier.hoon"       "$HOON_DIR/lib/vesl-verifier.hoon"

# vesl-stark-verifier.hoon is INTENTIONALLY vendored (not symlinked).
# It carries a local hoonc-strict-compile patch (the `?=(%& -.result)`
# narrowing at +verify) that was never merged into upstream Vesl. Any
# VESL_HOME checkout without the patch produces a nest-fail ~90
# seconds into the compile — see the header comment in the vendored
# file for the exact diff against upstream. When upstream merges the
# fix, delete the vendored copy and add the symlink back here.
if [[ -L "$HOON_DIR/lib/vesl-stark-verifier.hoon" ]]; then
    echo "  vesl-stark-verifier.hoon: replacing stale symlink with vendored copy"
    rm "$HOON_DIR/lib/vesl-stark-verifier.hoon"
    cp "$VESL_HOME/protocol/lib/vesl-stark-verifier.hoon" \
       "$HOON_DIR/lib/vesl-stark-verifier.hoon"
    echo "  vesl-stark-verifier.hoon: vendored from $VESL_HOME (was symlink)"
elif [[ ! -e "$HOON_DIR/lib/vesl-stark-verifier.hoon" ]]; then
    echo "Error: $HOON_DIR/lib/vesl-stark-verifier.hoon missing." >&2
    echo "       Vendored copy should be checked in. Restore from git." >&2
    exit 1
else
    echo "  vesl-stark-verifier.hoon: vendored (local patch; upstream PR pending)"
fi

echo "Done."
