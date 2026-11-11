#!/usr/bin/env bash
# One-time per clone: run the checks automatically before every commit.
set -euo pipefail
cd "$(dirname "$0")/.."
chmod +x .githooks/* scripts/check.sh
git config core.hooksPath .githooks
echo "Pre-commit checks enabled (.githooks/pre-commit → scripts/check.sh)."
