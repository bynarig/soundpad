#!/usr/bin/env bash
# Scan a release zip for malware before it's published.
#
#   scripts/virus_scan.sh Soundpad-X.Y.Z.zip report.md
#
# - ClamAV (always): scans the zip and the unzipped app, using the official
#   clamav/clamav Docker image, which ships with that day's virus signatures.
# - VirusTotal (when VIRUSTOTAL_API_KEY is set): uploads the zip and waits for
#   about 70 antivirus engines to check it.
#
# Writes a Markdown section for the release notes to report.md. Its last line
# is a hidden comment with the results as JSON, which the website reads.
#
# Exits 1 if ClamAV finds anything, or VIRUSTOTAL_BLOCK (default 3) or more
# VirusTotal engines call it malicious. Fewer than that is reported but doesn't
# block: a lone engine flagging a new, unnotarized app is a common false alarm.
set -euo pipefail

ZIP=${1:?usage: virus_scan.sh FILE.zip REPORT.md}
OUT=${2:?usage: virus_scan.sh FILE.zip REPORT.md}
BLOCK=${VIRUSTOTAL_BLOCK:-3}
IMAGE=clamav/clamav:stable

NAME=$(basename "$ZIP")
if command -v sha256sum >/dev/null; then SHA=$(sha256sum "$ZIP" | cut -d' ' -f1)
else SHA=$(shasum -a 256 "$ZIP" | cut -d' ' -f1); fi
DATE=$(date -u +%Y-%m-%d)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/files"
cp "$ZIP" "$WORK/files/"
unzip -q "$ZIP" -d "$WORK/files/unzipped"
FAILED=0

# ---- ClamAV ----
echo "▸ ClamAV"
mkdir "$WORK/out"
set +e
# Fetch today's signatures first; the image's own are up to a few days old.
docker run --rm --entrypoint sh -v "$WORK/files:/scan:ro" -v "$WORK/out:/out" "$IMAGE" -c '
  freshclam --stdout >/out/freshclam.log 2>&1 || echo "::warning::Could not update ClamAV signatures; using the ones in the image."
  clamscan --version > /out/version
  clamscan --recursive --infected --scan-archive=yes --alert-broken-media=no /scan' | tee "$WORK/clamav.log"
CLAM_EXIT=${PIPESTATUS[0]}
set -e
CLAM_VERSION=$(cat "$WORK/out/version")  # "ClamAV 1.5.4/28177/Sun Nov  8 06:23:52 2026"
echo "$CLAM_VERSION"
case $CLAM_EXIT in
  0) CLAM=clean; CLAM_LINE="no threats found" ;;
  1) CLAM=infected; FAILED=1
     CLAM_LINE="**threats found:** $(grep 'FOUND$' "$WORK/clamav.log" | sed 's#^/scan/##' | paste -sd ';' -)"
     echo "::error::ClamAV found threats in $NAME" ;;
  *) echo "::error::ClamAV couldn't run (exit $CLAM_EXIT)"; exit 2 ;;
esac
CLAM_VER=$(cut -d/ -f1 <<< "$CLAM_VERSION" | sed 's/^ClamAV //')
CLAM_DB=$(cut -d/ -f3- <<< "$CLAM_VERSION")

# ---- VirusTotal ----
VT_JSON='null'
VT_LINE=""
VT_URL="https://www.virustotal.com/gui/file/$SHA"
if [[ -n "${VIRUSTOTAL_API_KEY:-}" ]]; then
  echo "▸ VirusTotal"
  vt() { curl -fsS --retry 3 -H "x-apikey: $VIRUSTOTAL_API_KEY" "$@"; }
  API=https://www.virustotal.com/api/v3
  if (( $(wc -c < "$ZIP") > 32000000 )); then
    UPLOAD=$(vt "$API/files/upload_url" | jq -r .data)
  else
    UPLOAD="$API/files"
  fi
  ANALYSIS=$(vt -F "file=@$ZIP" "$UPLOAD" | jq -r .data.id)
  STATUS=queued
  for _ in $(seq 60); do            # up to 15 minutes
    sleep 15
    R=$(vt "$API/analyses/$ANALYSIS")
    STATUS=$(jq -r .data.attributes.status <<< "$R")
    echo "  $STATUS"
    [[ $STATUS == completed ]] && break
  done
  if [[ $STATUS == completed ]]; then
    MAL=$(jq '.data.attributes.stats.malicious' <<< "$R")
    SUS=$(jq '.data.attributes.stats.suspicious' <<< "$R")
    ENGINES=$(jq '.data.attributes.stats | .malicious + .suspicious + .undetected + .harmless' <<< "$R")
    FLAGGED=$(jq -r '[.data.attributes.results[] | select(.category == "malicious") | .engine_name] | join(", ")' <<< "$R")
    VT_JSON=$(jq -nc --argjson m "$MAL" --argjson s "$SUS" --argjson e "$ENGINES" --arg u "$VT_URL" \
      '{malicious:$m, suspicious:$s, engines:$e, url:$u}')
    if (( MAL == 0 )); then
      VT_LINE="0 of $ENGINES engines found anything"
    else
      VT_LINE="$MAL of $ENGINES engines flagged it ($FLAGGED)"
      echo "::warning::VirusTotal: $VT_LINE"
    fi
    (( SUS > 0 )) && VT_LINE+=", $SUS marked it suspicious"
    VT_LINE+=" · [full report]($VT_URL)"
    if (( MAL >= BLOCK )); then
      FAILED=1
      echo "::error::$MAL VirusTotal engines flagged $NAME (limit $BLOCK). Check the report: $VT_URL"
    fi
  else
    echo "::warning::VirusTotal didn't finish in time; the report will appear at $VT_URL"
    VT_LINE="still analyzing · [report]($VT_URL)"
  fi
else
  echo "▸ VirusTotal skipped (no VIRUSTOTAL_API_KEY)"
fi

# ---- Report ----
{
  echo "<!-- virus-scan-start -->"
  echo "**Virus scan** ($DATE) of \`$NAME\`"
  echo "- ClamAV $CLAM_VER (signatures from $CLAM_DB): $CLAM_LINE"
  [[ -n "$VT_LINE" ]] && echo "- VirusTotal: $VT_LINE"
  echo "- SHA-256: \`$SHA\`"
  echo
  echo "The scan runs automatically on GitHub for every release before it's published ([how](https://github.com/bynarig/soundpad/blob/main/.github/workflows/virus-scan.yml))."
  jq -nc --arg d "$DATE" --arg f "$NAME" --arg h "$SHA" --arg c "$CLAM" --arg cv "$CLAM_VER" --argjson vt "$VT_JSON" \
    '{date:$d, file:$f, sha256:$h, clamav:$c, clamav_version:$cv, virustotal:$vt}' \
    | sed 's/^/<!-- virus-scan: /; s/$/ -->/'
  echo "<!-- virus-scan-end -->"
} > "$OUT"
cat "$OUT"
exit $FAILED
