#!/usr/bin/env bash
# Offline native checks use generated pixels, never a desktop capture or input.
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/build-macos-source.sh
printf '%s\n' '{"protocol_version":1,"command":"self_test"}' | target/debug/cu-macos-source
