#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/build-macos-device.sh
target/debug/cu-macos-device-guard --self-test "$PWD/contracts/desktop-v1/native-binding/golden.json"
