#!/usr/bin/env bash
# Link nockchain hoon subtrees that the NNS kernel needs to compile
# under hoonc. Idempotent.
#
# Required for building with the STARK prover enabled (Phase 0):
#   - /common/v2/          prover/verifier tables
#   - /common/v0-v1/       proof-version routing
#   - /common/stark/       STARK prover + verifier arms
#   - /common/nock-verifier.hoon  `verify` gate (Phase 1-redo bench)
#   - /dat/softed-constraints  preprocessed constraints
#
# Locally vendored (untouched by this script):
#   - /common/wrapper.hoon
#   - /common/zeke.hoon
#   - /common/ztd/
#   - /lib/
#   - /app/
#   - /tests/
#
# Reads NOCK_HOME from:
#   1. env var, or
#   2. vesl.toml's nock_home field (default: ../nockchain)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOON_DIR="$REPO_ROOT/hoon"

if [[ -z "${NOCK_HOME:-}" ]]; then
    if [[ -f "$REPO_ROOT/vesl.toml" ]]; then
        NOCK_HOME="$(grep -s '^nock_home' "$REPO_ROOT/vesl.toml" | head -1 \
            | sed 's/.*= *"\(.*\)"/\1/')"
    fi
fi

if [[ -z "${NOCK_HOME:-}" ]]; then
    NOCK_HOME="../nockchain"
fi

# Resolve relative NOCK_HOME against repo root so symlinks are correct.
if [[ "$NOCK_HOME" != /* ]]; then
    NOCK_HOME="$(cd "$REPO_ROOT/$NOCK_HOME" 2>/dev/null && pwd)" || {
        echo "Error: NOCK_HOME resolves to a missing directory: $NOCK_HOME" >&2
        exit 1
    }
fi

if [[ ! -d "$NOCK_HOME/hoon/common/v2" ]]; then
    echo "Error: $NOCK_HOME/hoon/common/v2 not found." >&2
    echo "Is NOCK_HOME pointing to the nockchain monorepo root?" >&2
    exit 1
fi

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

echo "Linking nockchain hoon subtrees from $NOCK_HOME ..."
link "$NOCK_HOME/hoon/common/v2"       "$HOON_DIR/common/v2"
link "$NOCK_HOME/hoon/common/v0-v1"    "$HOON_DIR/common/v0-v1"
link "$NOCK_HOME/hoon/common/stark"    "$HOON_DIR/common/stark"
link "$NOCK_HOME/hoon/common/nock-verifier.hoon" \
  "$HOON_DIR/common/nock-verifier.hoon"
link "$NOCK_HOME/hoon/dat"             "$HOON_DIR/dat"
link "$NOCK_HOME/hoon/jams"            "$HOON_DIR/jams"
echo "Done."
