#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/debug target/native-macos/module-cache
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors -parse-as-library \
  -target arm64-apple-macosx14.0 -module-cache-path target/native-macos/module-cache \
  native/keychain/KeychainMain.swift -o target/debug/cu-macos-keychain
