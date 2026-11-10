#!/usr/bin/env bash
# Run the Swift unit tests (macos/Tests): compiles them with the app's Swift
# sources (minus the app entry point, SoundpadApp.swift) and the Rust core.
#   scripts/test_swift.sh
set -euo pipefail
cd "$(dirname "$0")/.."

MIN_MACOS="26.0"
OUT=target/swift-tests
mkdir -p "$OUT"

cargo build --release --lib

SOURCES=()
for f in macos/Sources/*.swift; do
  [[ "$(basename "$f")" == "SoundpadApp.swift" ]] || SOURCES+=("$f")
done

swiftc -parse-as-library -target "$(uname -m)-apple-macos$MIN_MACOS" \
  -import-objc-header macos/Bridge/soundpad.h \
  "${SOURCES[@]}" macos/Tests/*.swift \
  target/release/libsoundpad.a \
  -framework AppKit -framework SwiftUI -framework Carbon -framework CoreGraphics \
  -framework CoreAudio -framework AudioToolbox -framework Foundation -liconv \
  -o "$OUT/SwiftTests"

"$OUT/SwiftTests"
