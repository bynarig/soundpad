#!/usr/bin/env bash
# One-time: create a permanent code-signing identity for Soundpad builds.
#
# macOS remembers privacy permissions (microphone) per code signature. Ad-hoc
# signatures change with every build, so without this every rebuild and every
# auto-update would ask for microphone access again. A self-signed certificate
# gives every build the same identity. (It doesn't replace Apple notarization.)
#
# Creates ~/.config/soundpad/codesign.keychain-db (+ its password file) holding
# the identity "Soundpad Code Signing". Back up that folder together with your
# update signing key: a new identity means users are asked for permissions once more.
set -euo pipefail

NAME="Soundpad Code Signing"
DIR="${SOUNDPAD_CONFIG_DIR:-$HOME/.config/soundpad}"
KC="$DIR/codesign.keychain-db"
PASSFILE="$DIR/codesign.keychain-password"

if [[ -f "$KC" ]]; then
  echo "Already set up: $KC" >&2
  exit 0
fi
mkdir -p "$DIR"; chmod 700 "$DIR"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

PASS=$(/usr/bin/openssl rand -hex 24)
umask 077
printf '%s' "$PASS" > "$PASSFILE"

cat > "$TMP/cert.conf" <<CONF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
CONF

# The system LibreSSL writes PKCS#12 files the macOS keychain can import.
/usr/bin/openssl req -x509 -newkey rsa:3072 -sha256 -days 7300 -nodes \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -config "$TMP/cert.conf" 2>/dev/null
/usr/bin/openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -name "$NAME" -out "$TMP/id.p12" -passout "pass:$PASS" 2>/dev/null

security create-keychain -p "$PASS" "$KC"
security set-keychain-settings "$KC"          # never auto-lock
security unlock-keychain -p "$PASS" "$KC"
security import "$TMP/id.p12" -k "$KC" -P "$PASS" -T /usr/bin/codesign >/dev/null
# Let codesign use the key without a GUI prompt.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PASS" "$KC" >/dev/null

echo "Created code-signing identity \"$NAME\" in $KC"
echo "Back up $DIR (it also holds your update signing key)."
