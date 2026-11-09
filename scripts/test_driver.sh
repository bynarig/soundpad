#!/usr/bin/env bash
# Functional test of the virtual mic driver without installing it.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -d target/driver/SoundpadVirtualMic.driver ]] || scripts/build_driver.sh
clang -O1 -Wall -Wno-unused-parameter tests/driver/harness.c \
  -framework CoreAudio -framework CoreFoundation -o target/driver/harness
target/driver/harness target/driver/SoundpadVirtualMic.driver/Contents/MacOS/SoundpadVirtualMic
