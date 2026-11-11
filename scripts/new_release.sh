#!/usr/bin/env bash
# Cut a release: bump the version, commit, tag and push. GitHub Actions
# (.github/workflows/release.yml) then tests, builds, signs and publishes it.
#
#   scripts/new_release.sh 0.3.3 "Short summary" [notes.md]
#
# The summary is the first line of the release notes and the text shown in the
# app's update offer. An optional Markdown file adds the full "What's new".
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-}"; SUMMARY="${2:-}"; NOTES_FILE="${3:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && -n "$SUMMARY" ]] || {
  echo "usage: scripts/new_release.sh X.Y.Z \"Short summary\" [notes.md]" >&2; exit 1; }
TAG="v$VERSION"
CURRENT=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)

[[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] || { echo "Releases are cut from main." >&2; exit 1; }
git diff --quiet && git diff --cached --quiet -- . ':!AGENT.md' || {
  echo "Commit or stash your changes first." >&2; exit 1; }
git fetch --quiet origin main --tags
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || { echo "main isn't in sync with origin/main." >&2; exit 1; }
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && { echo "Tag $TAG already exists." >&2; exit 1; }
python3 - "$VERSION" "$CURRENT" <<'PY' || { echo "$VERSION must be newer than $CURRENT." >&2; exit 1; }
import sys
new, cur = (tuple(map(int, v.split("."))) for v in sys.argv[1:])
sys.exit(0 if new > cur else 1)
PY

sed -i '' "1,/^version = \"$CURRENT\"/s/^version = \"$CURRENT\"/version = \"$VERSION\"/" Cargo.toml
cargo update -p soundpad --offline --quiet

MSG=$(mktemp)
{ echo "$SUMMARY"; if [[ -n "$NOTES_FILE" ]]; then echo; cat "$NOTES_FILE"; fi; } > "$MSG"

git commit --quiet -m "$VERSION: $SUMMARY" Cargo.toml Cargo.lock
git tag -a "$TAG" -F "$MSG"
rm -f "$MSG"
git push --quiet origin main "$TAG"

echo "Pushed $TAG. GitHub Actions is building the release:"
echo "  https://github.com/$(python3 -c "import json;print(json.load(open('macos/Updates.json'))['repo'])")/actions/workflows/release.yml"
