#!/usr/bin/env bash
# Run the same checks as CI, before a commit.
#
#   scripts/check.sh            # only what the staged files need (used by the pre-commit hook)
#   scripts/check.sh --all      # everything
#
# Website/README-only commits skip straight through. To skip the hook once:
# `git commit --no-verify` (CI still runs everything on push).
set -euo pipefail
cd "$(dirname "$0")/.."

ALL=0
[[ "${1:-}" == "--all" ]] && ALL=1

if [[ $ALL == 1 ]]; then
  FILES="(all)"
else
  FILES=$(git diff --cached --name-only --diff-filter=ACMR)
fi
changed() { [[ $ALL == 1 ]] || grep -Eq "$1" <<< "$FILES"; }

RUST='^(src/|tests/.*\.rs$|Cargo\.(toml|lock)$|rustfmt\.toml$|build\.rs$)'
SWIFT='^(macos/(Sources|Tests|Bridge)/|src/ffi\.rs$|scripts/test_swift\.sh$)'
L10N='^(macos/Localization/|macos/Sources/|scripts/localize\.py$)'
DRIVER='^(vendor/blackhole-driver/|tests/driver/|scripts/(build|test)_driver\.sh$)'
SHELL_SCRIPTS='^(scripts/.*\.sh|\.githooks/)'

ran=0
step() { printf '\n\033[1m▸ %s\033[0m\n' "$1"; ran=1; }

if changed "$RUST"; then
  step "Rust format";  cargo fmt --check
  step "Rust lint";    cargo clippy --release --all-targets --quiet -- -D warnings
  step "Rust tests";   cargo test --release --quiet
fi
if changed "$SWIFT"; then
  step "Swift tests";  scripts/test_swift.sh
fi
if changed "$L10N"; then
  step "Translations"; python3 scripts/localize.py --check
fi
if changed "$DRIVER"; then
  step "Virtual mic driver"; scripts/test_driver.sh
fi
if changed "$SHELL_SCRIPTS"; then
  step "Shell scripts"
  for f in scripts/*.sh .githooks/*; do bash -n "$f"; done
  echo "syntax OK"
fi
if changed '^\.github/workflows/' && command -v actionlint >/dev/null; then
  step "Workflows"; actionlint
fi

if [[ $ran == 0 ]]; then
  echo "pre-commit: nothing to check for these files."
else
  printf '\n\033[32m✓ All checks passed.\033[0m\n'
fi
