#!/usr/bin/env bash
# Build zomes to WebAssembly and pack the DNA and hApp bundles.
# Run inside the dev shell:  nix develop -c ./build.sh
set -euo pipefail
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown \
  -p ledger_integrity -p ledger
hc dna pack dnas/dex -o dnas/dex/workdir/dex.dna
hc app pack workdir -o workdir/dex.happ
echo "Built workdir/dex.happ"
