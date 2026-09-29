#!/usr/bin/env bash
# Sandboxed conductors, one per agent (default 2: alice, bob), for driving the
# UI from a plain browser: one tab per agent, or the Playwright script. For
# the everyday windowed setup use `npm start` in ui/ (hc-spin) instead.
#
# Run inside the dev shell, after ./build.sh:
#   nix develop -c ./scripts/sandbox.sh               # 2 agents
#   AGENTS=4 nix develop -c ./scripts/sandbox.sh      # 4 agents
# then `npm run dev` in ui/ and open the URLs it prints.
# Ctrl-C stops everything; the sandbox data is deleted.
set -euo pipefail
cd "$(dirname "$0")/.."

AGENTS="${AGENTS:-2}"
NAMES=(alice bob carol dave erin frank grace heidi ivan judy)
((AGENTS >= 1 && AGENTS <= ${#NAMES[@]})) || { echo "AGENTS must be 1-${#NAMES[@]}" >&2; exit 1; }
DIRS="$(IFS=,; echo "${NAMES[*]:0:AGENTS}")"
# App interface ports 8801, 8802, … one per agent.
APP_PORTS="${APP_PORTS:-$(seq -s, 8801 $((8800 + AGENTS)))}"
UI_URL="${UI_URL:-http://localhost:8888}"
ROOT="$(mktemp -d -t dex-sandbox-XXXXXX)"
trap 'kill $(jobs -p) 2>/dev/null || true; wait 2>/dev/null; rm -rf "$ROOT"' EXIT

# One local server is both the bootstrap service and the iroh relay.
kitsune2-bootstrap-srv >"$ROOT/bootstrap.log" 2>&1 &
until grep -q '#kitsune2_bootstrap_srv#listening#' "$ROOT/bootstrap.log"; do sleep 0.2; done
SERVER="http://$(grep -o '#kitsune2_bootstrap_srv#listening#[^#]*' "$ROOT/bootstrap.log" | head -1 | sed 's/.*listening#//')"
echo "bootstrap + relay: $SERVER"

# Sandbox conductors ask for a lair passphrase; --piped reads it from stdin.
echo "sandbox-passphrase" | hc sandbox --piped generate \
  --root "$ROOT" -n "$AGENTS" -d "$DIRS" -a dex -r="$APP_PORTS" \
  workdir/dex.happ network -b "$SERVER" quic "$SERVER" 2>&1 |
  while IFS= read -r line; do
    echo "$line" >>"$ROOT/conductors.log"
    # hc prints one launch line per conductor with its admin and app ports.
    if [[ "$line" =~ \"admin_port\":([0-9]+).*\"app_ports\":\[([0-9]+) ]]; then
      echo "agent ready: $UI_URL/?admin_port=${BASH_REMATCH[1]}&app_port=${BASH_REMATCH[2]}"
    fi
  done
