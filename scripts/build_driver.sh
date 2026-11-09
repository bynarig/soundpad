#!/usr/bin/env bash
# Build the "Soundpad Virtual Mic" CoreAudio HAL plug-in from the vendored
# BlackHole source (GPL-3.0) into target/driver/SoundpadVirtualMic.driver.
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="Soundpad Virtual Mic"
BUNDLE_ID="dev.soundpad.virtualmic"
EXE="SoundpadVirtualMic"
SRC="vendor/blackhole-driver"
OUT="target/driver/$EXE.driver"
# A factory UUID of our own so it can coexist with a real BlackHole install.
FACTORY_UUID="7C1F3B0E-5A2D-4F43-9E6B-0D5A1C8B2F61"
VERSION="$(tr -d '[:space:]' < "$SRC/VERSION")"

rm -rf "$OUT"
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources"

# Upstream hard-codes a few user-visible "BlackHole" strings outside the
# customization macros; replace them in a build copy (vendored file untouched).
BUILD_SRC="target/driver/BlackHole-patched.c"
sed -e 's/CFSTR("BlackHole Box")/CFSTR(kDriver_Name " Box")/' \
    -e 's/CFSTR("BlackHole")/CFSTR(kDriver_Name)/' \
    -e 's/implemented by the BlackHole driver/implemented by the " kDriver_Name " driver/' \
    "$SRC/BlackHole.c" > "$BUILD_SRC"

clang -arch arm64 -arch x86_64 -mmacosx-version-min=11.0 -O2 -bundle \
  -Wno-deprecated-declarations \
  -DkDriver_Name="\"$NAME\"" \
  -DkPlugIn_BundleID="\"$BUNDLE_ID\"" \
  -DkPlugIn_Icon="\"$EXE.icns\"" \
  -DkHas_Driver_Name_Format=false \
  -DkDevice_Name="\"$NAME\"" \
  -DkDevice2_Name="\"$NAME (mirror)\"" \
  -DkManufacturer_Name="\"Soundpad (based on BlackHole by Existential Audio)\"" \
  -DkCanBeDefaultSystemDevice=false \
  -DkNumber_Of_Channels=2 \
  -framework CoreAudio -framework CoreFoundation -framework Accelerate \
  -Wno-format-extra-args \
  -o "$OUT/Contents/MacOS/$EXE" "$BUILD_SRC"

cat > "$OUT/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>English</string>
  <key>CFBundleExecutable</key><string>$EXE</string>
  <key>CFBundleIconFile</key><string>$EXE.icns</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleSignature</key><string>????</string>
  <key>CFPlugInFactories</key>
  <dict>
    <key>$FACTORY_UUID</key><string>BlackHole_Create</string>
  </dict>
  <key>CFPlugInTypes</key>
  <dict>
    <key>443ABAB8-E7B3-491A-B985-BEB9187030DB</key>
    <array><string>$FACTORY_UUID</string></array>
  </dict>
</dict>
</plist>
PLIST

cp "$SRC/LICENSE" "$OUT/Contents/Resources/LICENSE-GPL3.txt"
cp "$SRC/SOURCE.md" "$OUT/Contents/Resources/SOURCE.md"
if [[ -n "${ICON_ICNS:-}" && -f "$ICON_ICNS" ]]; then
  cp "$ICON_ICNS" "$OUT/Contents/Resources/$EXE.icns"
fi

codesign --force --sign - "$OUT" >/dev/null
echo "Built $OUT"
