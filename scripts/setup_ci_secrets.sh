#!/usr/bin/env bash
# One-time: store the release signing keys as encrypted GitHub Actions secrets,
# so .github/workflows/release.yml can sign releases like a local build does.
#
#   scripts/setup_ci_secrets.sh
#
# Uploads (with `gh secret set`, values are piped in, never printed):
#   SOUNDPAD_UPDATE_SIGNING_KEY          ~/.config/soundpad/update_signing_key
#   SOUNDPAD_CODESIGN_KEYCHAIN           ~/.config/soundpad/codesign.keychain-db (base64)
#   SOUNDPAD_CODESIGN_KEYCHAIN_PASSWORD  ~/.config/soundpad/codesign.keychain-password
#
# Anyone who can push tags to the repo can then publish signed updates.
set -euo pipefail
cd "$(dirname "$0")/.."

DIR="${SOUNDPAD_CONFIG_DIR:-$HOME/.config/soundpad}"
REPO=$(python3 -c "import json;print(json.load(open('macos/Updates.json'))['repo'])")
KEY="$DIR/update_signing_key"
KC="$DIR/codesign.keychain-db"
PASS="$DIR/codesign.keychain-password"

for f in "$KEY" "$KC" "$PASS"; do
  [[ -f "$f" ]] || { echo "Missing $f (see scripts/release.sh --setup and scripts/setup_codesign.sh)" >&2; exit 1; }
done

# Refuse to upload a key that doesn't match the one built into the app.
EXPECTED=$(python3 -c "import json;print(json.load(open('macos/Updates.json'))['publicKey'])")
ACTUAL=$(swift scripts/update_tool.swift public-key "$KEY")
[[ "$EXPECTED" == "$ACTUAL" ]] || { echo "update_signing_key doesn't match publicKey in macos/Updates.json" >&2; exit 1; }

echo "Storing release signing secrets in $REPO…"
gh secret set SOUNDPAD_UPDATE_SIGNING_KEY --repo "$REPO" < "$KEY"
base64 -i "$KC" | gh secret set SOUNDPAD_CODESIGN_KEYCHAIN --repo "$REPO"
gh secret set SOUNDPAD_CODESIGN_KEYCHAIN_PASSWORD --repo "$REPO" < "$PASS"
echo "Done. Secrets in $REPO:"
gh secret list --repo "$REPO"
