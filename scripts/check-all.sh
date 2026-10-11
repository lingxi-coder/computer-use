#!/usr/bin/env bash
# Every gate, cheapest first. Run from anywhere; it works from the repository root.
#
# Needs: the pinned Rust toolchain (rust-toolchain.toml), Node 22 or newer, and
#   ELECTRON_SKIP_BINARY_DOWNLOAD=1 npm ci
# having been run once. The Electron binary is not needed: nothing here opens a
# window. On a nearly full disk, `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0`
# keeps the build directory small.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

if [ ! -d node_modules ]; then
  echo "node_modules is missing: run  ELECTRON_SKIP_BINARY_DOWNLOAD=1 npm ci" >&2
  exit 1
fi

step "MVP business denominators and quality aggregation"
node --test scripts/mvp/business-results.test.mjs

step "pinned model dependency source identities"
bash scripts/check-model-source.sh

step "rust: format"
cargo fmt --all -- --check

step "rust: clippy (warnings are errors)"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "rust: tests (unit, contract, scenario, and real-process)"
cargo test --workspace --locked

step "generated TypeScript types are current with the published schema"
node scripts/gen-types.mjs --check

step "typescript: typecheck"
npm run typecheck

step "typescript: tests (they start the real native processes)"
npm test

step "typescript: production build"
npm run build

if [ "$(uname -s)" = Darwin ]; then
  step "macOS source: Swift 6 build and generated-image Vision checks"
  bash scripts/check-macos-source.sh
  step "macOS device: guard/atomic-submit Swift 6 build and safety checks"
  bash scripts/check-macos-device.sh
  step "macOS device: mocked blocked-submission and Stop isolation checks"
  bash scripts/check-macos-stop-gate.sh
  step "macOS Keychain: owned temporary test namespace roundtrip"
  bash scripts/check-macos-keychain.sh
fi

printf '\nall checks passed\n'
