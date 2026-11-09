# Vendored: BlackHole virtual audio driver

- Upstream: https://github.com/ExistentialAudio/BlackHole
- Tag: v0.7.1 — commit `e2b22aaaba4e507a097131704bf96dabc004d9cf`
- License: GPL-3.0 (see `LICENSE`). Copyright © 2019–2026 Existential Audio Inc.
- `BlackHole.c` is unmodified. `scripts/build_driver.sh` compiles it into a separate
  HAL plug-in named **Soundpad Virtual Mic** using the upstream-documented preprocessor
  customization constants.
- Per upstream's terms, the "BlackHole" name, logo and branding are **not** used for this build.

The driver is a separate program loaded by `coreaudiod`; it is not linked into Soundpad.
