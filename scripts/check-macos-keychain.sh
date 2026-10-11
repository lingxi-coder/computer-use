#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/build-macos-keychain.sh
target/debug/cu-macos-keychain --self-test
