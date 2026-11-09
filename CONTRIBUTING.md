# Contributing to Soundpad

Thanks for helping! Bug reports, ideas, translation fixes and code are all welcome.

- **Bugs and ideas:** [open an issue](https://github.com/bynarig/soundpad/issues). For bugs,
  include your macOS version, your Mac model, what you did, what you expected, and what
  happened. If audio is involved, the output of the self-test helps a lot (see [Tests](#tests)).
- **Translations:** see [Fixing or adding a translation](#fixing-or-adding-a-translation).
  No programming needed.
- **Code:** fork the repo, create a branch, and open a pull request against `main`.

By contributing, you agree that your contribution is licensed under the project's
[GPL-3.0 license](LICENSE).

---

## Build and run

You need macOS 26 or later, Xcode (for the Swift compiler and SDK), Rust (`rustup`) and Python 3
(included with Xcode's command-line tools).

```sh
scripts/bundle.sh                   # builds dist/Soundpad.app
open dist/Soundpad.app
scripts/bundle.sh --install         # ...and copies it to /Applications
scripts/bundle.sh --universal       # arm64 + x86_64 (rustup target add x86_64-apple-darwin)
```

The library and settings live in `~/Library/Application Support/Soundpad/soundpad.json`.
To keep testing separate from your real library, set `SOUNDPAD_CONFIG=/path/to/test.json`.
Set `SOUNDPAD_MUTE=1` to render audio silently.

## How the code is organized

| Path | What's there |
|---|---|
| `macos/Sources/` | The SwiftUI app (Liquid Glass design): window, sidebar, player bar, Settings, hotkey recorder, updater |
| `macos/Bridge/soundpad.h` | The small C API the Swift app uses to talk to the Rust core |
| `macos/Localization/` | Translations: `keys.json` (English) and one `<language>.json` file per language |
| `macos/Updates.json` | Update feed URL and the public key that updates must be signed with |
| `src/` | The Rust core, built as a static library |
| `src/audio/` | Decoding, streaming, the real-time mixer, CoreAudio I/O and the player |
| `src/core.rs` | Library, playlists, settings and devices. The UI sends it JSON commands (`Command`) and gets JSON state back |
| `src/hotkeys.rs` | System-wide hotkeys (no Accessibility permission needed) |
| `src/virtual_mic.rs` | Installs and uninstalls the virtual mic driver |
| `vendor/blackhole-driver/` | Unmodified BlackHole source, built into *Soundpad Virtual Mic* |
| `scripts/` | Build, release, translation and driver scripts |
| `tests/` | Integration tests |

### Real-time audio rules

Each output device has its own mixer running inside the CoreAudio callback
(`src/audio/mixer.rs`). Code that runs there **must not** take locks, allocate or free memory,
do file or network I/O, or make system calls. Doing any of those causes audible dropouts.

- Commands reach the mixer through lock-free ring buffers. Memory the mixer is done with goes
  back through a "garbage" queue and is freed on a normal thread.
- Anything that needs memory (opening a stream, creating a voice) happens before the command is
  sent; see `Command::play` and `Command::seek`.
- `tests/realtime.rs` enforces this with a counting allocator. If you touch the mixer, run it.

### Memory and streaming

- Sounds are stored as 16-bit PCM. Mono files stay mono.
- Clips of 10 seconds or less are fully decoded into memory, so they start instantly.
- Longer sounds keep only their first 1.5 s in memory. While one plays, a background thread
  (`src/audio/stream.rs`) decodes about 2 s ahead into a ring buffer. Seeking far ahead jumps
  in the file instead of decoding everything up to that point.

## Tests

```sh
cargo test --release                                    # all Rust tests
cargo test --release --test realtime -- --nocapture     # also prints performance numbers
scripts/test_driver.sh                                  # virtual mic driver, no install needed
dist/Soundpad.app/Contents/MacOS/Soundpad --selftest [--with-mic]   # silent test on your real devices
python3 scripts/localize.py --check                     # translations
```

- `tests/realtime.rs`: the audio callback makes **zero heap allocations**, including while
  streaming and seeking; 64 resampled voices plus mic passthrough fit easily in the time budget.
- `tests/stream.rs`: streamed playback (also after seeking and when looping) is bit-identical
  to playing a fully loaded copy.
- `tests/decode.rs`: decodes every macOS system sound, plus mp3/flac/ogg/m4a/aiff/caf files
  made with `ffmpeg` / `afconvert` when available.
- `tests/driver/harness.c`: loads the compiled driver the way `coreaudiod` does and checks that
  audio comes back bit-exact.
- `--selftest`: plays silently on your real devices and measures latency and callback cost.
  When the virtual mic is installed, it also checks what other apps receive (level, pitch,
  dropouts). Close Soundpad before running it, because its mic passthrough would mix into the
  test signal.
- `SOUNDPAD_LOG=1` prints every global hotkey press and how long it took.

Before opening a pull request, please run `cargo test --release` and
`cargo clippy --release --all-targets`, and build the app with `scripts/bundle.sh`.

## Fixing or adding a translation

All text lives in `macos/Localization/`:

- `keys.json` maps short IDs to the English text.
- `uk.json`, `de.json`, `ja.json`, … map the same IDs to translations.

**To fix a translation,** edit the value in that language's file and keep:

- placeholders exactly as they are: `%@` (a name or number) and `%lld` (a number),
- `**bold**` markers and `\n` line breaks,
- the names *Soundpad* and *Soundpad Virtual Mic* in English.

**To add a language,** copy an existing file to `<code>.json` (for example `nb.json` for
Norwegian Bokmål) and translate every value. The language code must be one macOS uses.

Check your work with:

```sh
python3 scripts/localize.py --check
```

The build fails if any language is missing a string or changes a placeholder, so you can't
accidentally break the app.

**Adding new text in Swift:** use plain string literals in SwiftUI views (`Text("…")`,
`Button("…")`), and `String(localized: "…")` anywhere else. Then add the English text to
`keys.json` and a translation to every language file. To list the strings the compiler sees, run:

```sh
swiftc … -emit-localized-strings -emit-localized-strings-path /tmp/strings
```

Messages that come from the Rust core are English. They are translated on the Swift side in
`macos/Sources/Localization.swift`. If you add a new core message, add it there and to the
translation files.

## Releasing a new version (maintainers)

Updates are delivered through GitHub Releases. The app reads
`https://github.com/bynarig/soundpad/releases/latest/download/appcast.json`, and only installs
a zip whose Ed25519 signature matches the public key in `macos/Updates.json`.

One-time setup, already done by the maintainer:

```sh
scripts/release.sh --setup   # creates ~/.config/soundpad/update_signing_key (back it up!)
```

For each release:

1. Bump `version` in `Cargo.toml` (for example 0.1.0 → 0.2.0) and commit.
2. Run `scripts/release.sh "What's new"`. Add `--universal` to include Intel Macs.
   This creates `dist/release/Soundpad-X.Y.Z.zip` and `dist/release/appcast.json`.
3. Publish both files with the `gh release create vX.Y.Z …` command the script prints.

Never commit the signing key. Anyone who has it can ship updates to every user.
