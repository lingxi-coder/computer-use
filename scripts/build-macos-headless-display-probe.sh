#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/native-macos/module-cache
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors \
  -target arm64-apple-macosx14.0 -module-cache-path target/native-macos/module-cache \
  native/headless/fixtures/DisplayProbe.swift -o target/native-macos/headless-display-probe
