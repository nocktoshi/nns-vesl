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
# files to symlinks so we track vesl master exactly.
#
# Locally owned (untouched by this script):
#   - /common/wrapper.hoon
#   - /common/zeke.hoon           (a stub in hoon/common that /+-references zeke)
#   - /common/ztd/
#   - /lib/ (empty; everything lives upstream now)
#   - /app/
#   - /tests/
#
# Env vars (first non-empty wins; if both missing, falls back to the
# sibling-clone defaults `../nockchain` and `../vesl`):
#   - NOCK_HOME / vesl.toml's `nock_home`
#   - VESL_HOME / vesl.toml's `vesl_home`
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOON_DIR="$REPO_ROOT/hoon"

read_toml_key() {
    local key="$1"
    if [[ -f "$REPO_ROOT/vesl.toml" ]]; then
        # `grep -s` with no match exits nonzero under `set -e`; swallow.
        grep -s "^${key}" "$REPO_ROOT/vesl.toml" 2>/dev/null \
            | head -1 \
            | sed 's/.*= *"\(.*\)"/\1/' \
            || true
    fi
}

if [[ -z "${NOCK_HOME:-}" ]]; then
    NOCK_HOME="$(read_toml_key nock_home)"
fi
if [[ -z "${NOCK_HOME:-}" ]]; then
    NOCK_HOME="../nockchain"
fi

if [[ -z "${VESL_HOME:-}" ]]; then
    VESL_HOME="$(read_toml_key vesl_home)"
fi
if [[ -z "${VESL_HOME:-}" ]]; then
    VESL_HOME="../vesl"
fi

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

link() {
    local target="$1"
    local dest="$2"
    if [[ -L "$dest" ]]; then
        local current
        current="$(readlink "$dest")"
        if [[ "$current" == "$target" ]]; then
            echo "  $(basename "$dest"): already linked"
            return
        fi
        rm "$dest"
    elif [[ -e "$dest" ]]; then
        echo "Error: $dest exists and is not a symlink. Refusing to clobber." >&2
        exit 1
    fi
    ln -s "$target" "$dest"
    echo "  $(basename "$dest") -> $target"
}

mkdir -p "$HOON_DIR/lib"

echo "Linking nockchain hoon subtrees from $NOCK_HOME ..."
link "$NOCK_HOME/hoon/common/v2"                 "$HOON_DIR/common/v2"
link "$NOCK_HOME/hoon/common/v0-v1"              "$HOON_DIR/common/v0-v1"
link "$NOCK_HOME/hoon/common/stark"              "$HOON_DIR/common/stark"
link "$NOCK_HOME/hoon/common/nock-verifier.hoon" "$HOON_DIR/common/nock-verifier.hoon"
link "$NOCK_HOME/hoon/dat"                       "$HOON_DIR/dat"
link "$NOCK_HOME/hoon/jams"                      "$HOON_DIR/jams"

echo "Linking vesl hoon libs from $VESL_HOME ..."
link "$VESL_HOME/protocol/lib/vesl-graft.hoon"          "$HOON_DIR/lib/vesl-graft.hoon"
link "$VESL_HOME/protocol/lib/vesl-merkle.hoon"         "$HOON_DIR/lib/vesl-merkle.hoon"
link "$VESL_HOME/protocol/lib/vesl-prover.hoon"         "$HOON_DIR/lib/vesl-prover.hoon"
link "$VESL_HOME/protocol/lib/vesl-stark-verifier.hoon" "$HOON_DIR/lib/vesl-stark-verifier.hoon"
link "$VESL_HOME/protocol/lib/vesl-verifier.hoon"       "$HOON_DIR/lib/vesl-verifier.hoon"

echo "Done."
