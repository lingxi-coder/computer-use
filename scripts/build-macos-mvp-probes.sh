#!/usr/bin/env bash
# Read-only probes for freshly owned fixture processes; no commit entry point.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/native-macos/probe-module-cache
device_sources=()
for source_path in native/device/*.swift; do
  case "$source_path" in
    */DeviceMain.swift|*/SelfTest.swift|*Tests.swift) continue ;;
  esac
  device_sources+=("$source_path")
done
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors -parse-as-library \
  -target arm64-apple-macosx14.0 -module-cache-path target/native-macos/probe-module-cache \
  "${device_sources[@]}" native/mvp-fixture/HitProbe.swift \
  -o target/native-macos/mvp-fixture-hit-probe
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors -parse-as-library \
  -target arm64-apple-macosx14.0 -module-cache-path target/native-macos/probe-module-cache \
  native/mvp-fixture/ReadProbe.swift -o target/native-macos/mvp-fixture-read-probe
