#!/usr/bin/env bash
# Pure mocked submission concurrency checks. This binary cannot run the Device CLI.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/debug target/native-macos/module-cache
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors -parse-as-library \
  -target arm64-apple-macosx14.0 -module-cache-path target/native-macos/module-cache \
  -D CU_STOP_GATE_SELF_TEST native/device/*.swift -o target/debug/cu-macos-device-stop-gate-self-test
target/debug/cu-macos-device-stop-gate-self-test --self-test-stop-gate
