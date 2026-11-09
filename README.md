# Soundpad for macOS

A minimal, low-latency soundboard for macOS in the spirit of Soundpad for Windows:
play sounds with global hotkeys into your voice chat (Discord, Zoom, Teams, games)
while hearing them yourself.

## Features

- Sound list with search, drag-to-reorder, rename and remove; drag files or folders onto the window
- Plays MP3, WAV, FLAC, OGG/Vorbis, AAC/M4A, AIFF and CAF
- **Global hotkeys** for each sound, plus Stop and Pause. They work while the app is in the
  background and need no Accessibility permission.
- Two outputs at once: your **speakers/headphones** and a **virtual microphone**, each with its own volume and on/off toggle
- **Mic passthrough**: your real microphone is mixed into the virtual mic so people still hear your voice
- Choose whether a new sound replaces the current one (default) or overlaps it
- Automatically reconnects when audio devices change and follows the system default output

## Sending sounds to other apps (built-in virtual microphone)

Soundpad ships its own virtual audio device, **Soundpad Virtual Mic**, so nothing else needs to be installed:

1. Click **Install Soundpad Virtual Mic** in the banner, or use ⚙ Settings → Built-in virtual mic.
   macOS asks for your password once. Audio pauses for about a second while CoreAudio restarts.
   Soundpad then picks the device automatically.
2. Optionally set **Mix in my real mic** so your voice goes through too.
3. In Discord/Zoom/Teams/games, choose **Soundpad Virtual Mic** as your microphone.
   - Turn off the chat app's noise suppression / "Krisp" if sounds get cut off.

From a terminal: `Soundpad.app/Contents/MacOS/Soundpad --install-virtual-mic` (or `--uninstall-virtual-mic`).

The driver is built from the open-source [BlackHole](https://github.com/ExistentialAudio/BlackHole)
v0.7.1 (GPL-3.0, © Existential Audio). It is a separate plug-in loaded by `coreaudiod`, renamed
as their license terms require. The source and license are in `vendor/blackhole-driver/` and inside
the installed driver bundle. Other loopback devices (BlackHole, Loopback…) also work: pick them
under ⚙ Virtual microphone.

## Build & run

Requires macOS 26 or later (Liquid Glass), Xcode, and Rust.

```sh
scripts/bundle.sh                   # builds dist/Soundpad.app
scripts/bundle.sh --install         # ...and copies it to /Applications
scripts/bundle.sh --universal       # arm64 + x86_64 (rustup target add x86_64-apple-darwin)
open dist/Soundpad.app
```

### Architecture

- **`macos/`**: the native SwiftUI app with Apple's Liquid Glass design. It has a glass
  toolbar and search, a floating glass player bar (`GlassEffectContainer`), a glass setup
  banner, a glass keycap hotkey recorder, and a native Settings window.
- **`src/`**: the Rust core, built as a static library. It contains the real-time mixer,
  CoreAudio I/O, decoding, the library and settings, global hotkeys, and the driver
  installer. The SwiftUI app reaches it through a small C API (`src/ffi.rs`,
  `macos/Bridge/soundpad.h`): JSON commands and state, plus a cheap per-frame playback query.
- **`vendor/blackhole-driver/`**: the virtual mic driver source.

The library and settings are saved in `~/Library/Application Support/Soundpad/soundpad.json`.

## Keyboard (inside the window)

| Key | Action |
|---|---|
| ↑ / ↓ | select |
| Return / double-click | play |
| Space | pause / resume |
| Esc or ⌘. | stop |
| ⌫ | remove selected |
| ⌘O | add sounds |
| ⌘, | settings |
| Right-click | set hotkey, rename, show in Finder… |

## Why it doesn't lag

- Every sound is decoded into memory in the background when it's added, so triggering never touches the disk.
- Each output device has its own mixer running in the CoreAudio real-time callback. The mixer
  takes no locks, never allocates or frees memory, and makes no system calls. Commands arrive
  through lock-free ring buffers, and finished sounds are freed on another thread.
- Callbacks are small: 256 frames, about 5 ms. Hotkeys go straight to the mixers without waiting for the UI.
- Fades of 6 ms on stop, pause and replace avoid clicks, and a soft limiter prevents harsh clipping.
- Any sample rate is resampled with cubic interpolation. Mic passthrough tolerates clock drift
  between devices and keeps latency between 15 and 60 ms.
- The UI only redraws while something changes, so it uses about 0% CPU when idle.

## Updates

Soundpad updates itself from GitHub Releases of `bynarig/soundpad` (Settings → Updates, or
Soundpad ▸ Check for Updates…; it also checks quietly once a day). Every update zip is signed
with an Ed25519 key, and the app installs it only if the signature matches the public key
built into it (`macos/Updates.json`).

One-time setup, already done on the original machine:

```sh
scripts/release.sh --setup   # creates ~/.config/soundpad/update_signing_key (back it up!)
```

To ship a new version:

1. Bump `version` in `Cargo.toml` (for example 0.1.0 → 0.2.0).
2. `scripts/release.sh "What's new in this version"` (add `--universal` for Intel Macs too).
   This creates `dist/release/Soundpad-X.Y.Z.zip` and `dist/release/appcast.json`.
3. Publish both files in a release tagged `vX.Y.Z` (the script prints the `gh release create` command).
   The repository must be public so the app can download without logging in.

Keep the signing key safe. If it is lost, existing installs can no longer be updated, and anyone
who has it can ship updates to your users.

## Tests

```sh
cargo test                          # unit + integration tests
cargo test --release --test realtime -- --nocapture   # perf numbers
scripts/test_driver.sh              # virtual mic driver test, no install needed
dist/Soundpad.app/Contents/MacOS/Soundpad --selftest [--with-mic]  # silent test on real devices
```

- `tests/realtime.rs` checks that the audio callback performs **zero heap allocations**,
  using a counting global allocator. It also checks that 64 resampled voices plus mic
  passthrough stay well within the callback time budget.
- `tests/decode.rs` decodes every macOS system sound, plus files encoded to
  mp3/flac/ogg/m4a/aiff/caf by `ffmpeg` and `afconvert` when those tools are available.
- `tests/driver/harness.c` loads the compiled driver the way `coreaudiod` does, with a mock
  host. It checks the device names and flags, and that audio written to the device's output
  comes back from its input bit-exact, including across the ring-buffer wrap-around.
- `--selftest` opens your real devices at volume 0. It measures trigger-to-render latency and
  callback cost against the buffer size the device actually uses, and checks for missed
  callbacks under a 40-voice stress load.
  - When Soundpad Virtual Mic is installed, it also runs a **loopback test**. A tone is played
    into the device and recorded from its microphone side, exactly as Discord would receive it.
    The test checks level, pitch, dropouts and latency.
  - `--with-mic` also checks that your real microphone is mixed into the virtual mic
    continuously. The built-in mic is silent while the MacBook lid is closed, so the test
    then tries other inputs.
- `SOUNDPAD_LOG=1` prints every global hotkey press and how long it took to handle.
