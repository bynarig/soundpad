#!/usr/bin/env bash
# Build Soundpad.app into ./dist: native SwiftUI (Liquid Glass) front end
# linked against the Rust core (static library), plus the virtual mic driver.
#   scripts/bundle.sh              # this Mac's architecture
#   scripts/bundle.sh --universal  # arm64 + x86_64 (rustup target add x86_64-apple-darwin)
#   scripts/bundle.sh --install    # also copy to /Applications
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Soundpad"
BUNDLE_ID="dev.soundpad.mac"
MIN_MACOS="26.0"   # Liquid Glass
VERSION=${SOUNDPAD_VERSION:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}
# Auto-update feed and the public key that update downloads must be signed with.
UPDATE_FEED=${SOUNDPAD_UPDATE_FEED:-$(python3 -c "import json;print(json.load(open('macos/Updates.json'))['feedURL'])")}
UPDATE_KEY=$(python3 -c "import json;print(json.load(open('macos/Updates.json'))['publicKey'])")
APP="dist/$APP_NAME.app"
UNIVERSAL=0
INSTALL=0
for a in "$@"; do
  case "$a" in
    --universal) UNIVERSAL=1 ;;
    --install) INSTALL=1 ;;
  esac
done

FRAMEWORKS=(-framework AppKit -framework SwiftUI -framework Carbon -framework CoreGraphics
            -framework CoreAudio -framework AudioToolbox -framework Foundation -liconv)

build_arch() { # $1 = rust target triple or "", $2 = swift arch, $3 = output
  local libdir=target/release
  if [[ -n "$1" ]]; then
    cargo build --release --lib --target "$1"
    libdir="target/$1/release"
  else
    cargo build --release --lib
  fi
  swiftc -O -parse-as-library -target "$2-apple-macos$MIN_MACOS" \
    -import-objc-header macos/Bridge/soundpad.h \
    macos/Sources/*.swift \
    "$libdir/libsoundpad.a" "${FRAMEWORKS[@]}" \
    -o "$3"
}

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [[ $UNIVERSAL == 1 ]]; then
  T=$(mktemp -d)
  build_arch aarch64-apple-darwin arm64 "$T/arm64"
  build_arch x86_64-apple-darwin x86_64 "$T/x86_64"
  lipo -create -output "$APP/Contents/MacOS/$APP_NAME" "$T/arm64" "$T/x86_64"
  rm -rf "$T"
else
  build_arch "" "$(uname -m)" "$APP/Contents/MacOS/$APP_NAME"
fi

# Icon
ICON_WORK=$(mktemp -d)
swift scripts/make_icon.swift "$ICON_WORK/icon.png" >/dev/null
mkdir -p "$ICON_WORK/AppIcon.iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s "$ICON_WORK/icon.png" --out "$ICON_WORK/AppIcon.iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d "$ICON_WORK/icon.png" --out "$ICON_WORK/AppIcon.iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICON_WORK/AppIcon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICON_WORK"

# Built-in virtual microphone driver (installed on demand from the app).
ICON_ICNS="$APP/Contents/Resources/AppIcon.icns" scripts/build_driver.sh
cp -R target/driver/SoundpadVirtualMic.driver "$APP/Contents/Resources/"

# Translations (validated; fails the build on missing keys or broken placeholders).
LANGS=$(python3 scripts/localize.py "$APP/Contents/Resources")
LOCALIZATIONS=""
for l in $LANGS; do LOCALIZATIONS+="<string>$l</string>"; done

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>$APP_NAME</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleLocalizations</key><array>$LOCALIZATIONS</array>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>SoundpadUpdateFeedURL</key><string>$UPDATE_FEED</string>
  <key>SoundpadUpdatePublicKey</key><string>$UPDATE_KEY</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
  <key>NSHighResolutionCapable</key><true/>
  <!-- Hotkeys must fire instantly even when the app is in the background. -->
  <key>LSAppNapIsDisabled</key><true/>
  <key>NSMicrophoneUsageDescription</key>
  <string>Soundpad mixes your microphone into the virtual microphone so people hear your voice together with your sounds.</string>
</dict>
</plist>
PLIST

# Ad-hoc signature (needed for the microphone permission prompt on Apple Silicon).
codesign --force --deep --sign - "$APP"

echo "Built $APP"
if [[ $INSTALL == 1 ]]; then
  rm -rf "/Applications/$APP_NAME.app"
  cp -R "$APP" /Applications/
  echo "Installed to /Applications/$APP_NAME.app"
fi
