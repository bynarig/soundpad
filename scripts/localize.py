#!/usr/bin/env python3
"""Validate translations and write <lang>.lproj folders into the app bundle.

macos/Localization/keys.json   id -> English text (the localization key)
macos/Localization/<lang>.json id -> translation

  scripts/localize.py --check                 # validate only
  scripts/localize.py <Resources dir>         # validate + write .lproj folders,
                                              # prints the language codes

Checks: every language has exactly the ids of keys.json, and each translation
keeps the same placeholders (%@, %lld), **bold** markers and line breaks.
"""
import json
import plistlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "macos" / "Localization"
# Keys that live in Info.plist (InfoPlist.strings) instead of Localizable.strings.
INFO_PLIST = {"mic_usage": "NSMicrophoneUsageDescription"}
PLACEHOLDER = re.compile(r"%(?:\d+\$)?(?:@|lld|ld|d|lf|f)")


def signature(s: str):
    return (sorted(re.sub(r"\d+\$", "", p) for p in PLACEHOLDER.findall(s)), s.count("**"), s.count("\n"))


def load():
    keys = json.loads((ROOT / "keys.json").read_text())
    langs = {}
    errors = []
    for f in sorted(ROOT.glob("*.json")):
        if f.name == "keys.json":
            continue
        try:
            t = json.loads(f.read_text())
        except json.JSONDecodeError as e:
            errors.append(f"{f.name}: {e}")
            continue
        lang = f.stem
        missing = keys.keys() - t.keys()
        extra = t.keys() - keys.keys()
        if missing:
            errors.append(f"{lang}: missing {sorted(missing)}")
        if extra:
            errors.append(f"{lang}: unknown {sorted(extra)}")
        for k, v in t.items():
            if k in keys and signature(v) != signature(keys[k]):
                errors.append(f"{lang}.{k}: placeholders differ: {v!r}")
            if k in keys and not v.strip():
                errors.append(f"{lang}.{k}: empty")
        langs[lang] = t
    return keys, langs, errors


def main():
    keys, langs, errors = load()
    if errors:
        print("\n".join(errors), file=sys.stderr)
        sys.exit(1)
    if len(sys.argv) > 1 and sys.argv[1] == "--check":
        print(f"{len(langs)} languages × {len(keys)} strings OK", file=sys.stderr)
        return
    out = Path(sys.argv[1])
    for lang, t in {"en": {}, **langs}.items():
        d = out / f"{lang}.lproj"
        d.mkdir(parents=True, exist_ok=True)
        strings = {keys[k]: v for k, v in t.items() if k not in INFO_PLIST}
        info = {INFO_PLIST[k]: v for k, v in t.items() if k in INFO_PLIST}
        # .strings files may be property lists; binary is compact and fast to load.
        (d / "Localizable.strings").write_bytes(plistlib.dumps(strings, fmt=plistlib.FMT_BINARY))
        (d / "InfoPlist.strings").write_bytes(plistlib.dumps(info, fmt=plistlib.FMT_BINARY))
    print(" ".join(["en", *langs]))


if __name__ == "__main__":
    main()
