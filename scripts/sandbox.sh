#!/usr/bin/env bash
# Two sandboxed conductors (Alice and Bob) for driving the UI from a plain
# browser: two tabs, or the Playwright script. For the everyday two-window
# setup use `npm start` in ui/ (hc-spin) instead.
#
# Run inside the dev shell, after ./build.sh:
#   nix develop -c ./scripts/sandbox.sh
# then `npm run dev` in ui/ and open the two URLs it prints.
# Ctrl-C stops everything; the sandbox data is deleted.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_PORTS="${APP_PORTS:-8801,8802}"
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
  --root "$ROOT" -n 2 -d alice,bob -a dex -r="$APP_PORTS" \
  workdir/dex.happ network -b "$SERVER" quic "$SERVER" 2>&1 |
  while IFS= read -r line; do
    echo "$line" >>"$ROOT/conductors.log"
    # hc prints one launch line per conductor with its admin and app ports.
    if [[ "$line" =~ \"admin_port\":([0-9]+).*\"app_ports\":\[([0-9]+) ]]; then
      echo "agent ready: $UI_URL/?admin_port=${BASH_REMATCH[1]}&app_port=${BASH_REMATCH[2]}"
    fi
  done
