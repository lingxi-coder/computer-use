#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
fixture_app='target/native-macos/Interaction Fixture.app'
mkdir -p "$fixture_app/Contents/MacOS" target/native-macos/module-cache
xcrun swiftc -swift-version 6 -strict-concurrency=complete -warnings-as-errors -parse-as-library \
  -target arm64-apple-macosx14.0 -module-cache-path target/native-macos/module-cache \
  native/mvp-fixture/MvpFixture.swift -o "$fixture_app/Contents/MacOS/MvpFixture"
cat > "$fixture_app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>MvpFixture</string>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.mvp-fixture</string>
<key>CFBundleName</key><string>Interaction Fixture</string>
<key>CFBundleDisplayName</key><string>Interaction Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>1.0.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>NSHighResolutionCapable</key><true/>
<key>LSMinimumSystemVersion</key><string>14.0</string>
</dict></plist>
PLIST
plutil -lint "$fixture_app/Contents/Info.plist"
