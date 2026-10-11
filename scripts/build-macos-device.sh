#!/usr/bin/env bash
# Native device guards; AX submission requires an explicit fixed launch scope.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/debug target/native-macos/module-cache
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors -parse-as-library \
  -target arm64-apple-macosx14.0 -module-cache-path target/native-macos/module-cache \
  native/device/*.swift -o target/debug/cu-macos-device-guard
