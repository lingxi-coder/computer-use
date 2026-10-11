#!/usr/bin/env bash
# Build the source-only helper. No permissions are requested and no input is sent.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
  echo 'macOS is required for the native source helper' >&2
  exit 1
fi
mkdir -p target/debug target/native-macos/module-cache
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors -parse-as-library \
  -target arm64-apple-macosx14.0 \
  -module-cache-path target/native-macos/module-cache \
  native/macos/*.swift -o target/debug/cu-macos-source
