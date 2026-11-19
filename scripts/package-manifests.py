#!/usr/bin/env python3
"""Render and validate the Homebrew cask and WinGet manifests for a release.

The templates live in packaging/homebrew-tap/ and packaging/winget/. This
fills in the version, download URLs and SHA-256 checksums from the release
artifacts, then checks the result. Nothing here publishes anything: the
output is a directory to copy into talos-hq/homebrew-tap and a
winget-pkgs pull request once the artifacts are signed and the owner
approves.

  render   --version 1.2.3 --dmg PATH [--windows-installer PATH] --out DIR
  validate DIR

`render --dry-run` fills any missing artifact checksum with zeros so the
templates can be validated without a release (CI does this on every run).

`validate` checks each WinGet YAML file against Microsoft's JSON schema for
its ManifestVersion (fetched from microsoft/winget-cli at a pinned commit,
since `winget validate` only runs on Windows), checks the WinGet files agree
with each other, and runs `brew style` + `brew audit --cask --strict` on the
cask when Homebrew is installed (it is on GitHub's Ubuntu runners), or
`ruby -c` when only Ruby is. Needs PyYAML and jsonschema.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CASK_TEMPLATE = ROOT / "packaging/homebrew-tap/Casks/voxtype.rb.in"
WINGET_TEMPLATES = ROOT / "packaging/winget"

DEFAULT_REPO = "talos-hq/talos-vox"
WINGET_ID = "TalosHQ.Voxtype"
TAP = "talos-hq/tap"
CASK = "voxtype"

# microsoft/winget-cli commit the schemas are read from. Bump deliberately.
SCHEMA_COMMIT = "c09efb87d20b7b09a692aabfd1d55233733fea5f"
SCHEMA_URL = (
    "https://raw.githubusercontent.com/microsoft/winget-cli/{commit}"
    "/schemas/JSON/manifests/v{ver}/manifest.{kind}.{ver}.json"
)

ZERO_SHA = "0" * 64
PLACEHOLDER = re.compile(r"@[A-Z0-9_]+@")


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fill(template: str, values: dict[str, str]) -> str:
    out = template
    for key, value in values.items():
        out = out.replace("@" + key + "@", value)
    leftover = PLACEHOLDER.search(out)
    if leftover:
        raise SystemExit(f"unfilled placeholder {leftover.group(0)}")
    return out


def render(args: argparse.Namespace) -> int:
    version = args.version.removeprefix("v")
    out = Path(args.out)
    repo = args.repo
    base = f"https://github.com/{repo}/releases/download/v{version}"

    def checksum(path: str | None, what: str) -> str | None:
        if path:
            return sha256(Path(path))
        if args.dry_run:
            print(f"dry run: no {what}, using a zero checksum")
            return ZERO_SHA
        return None

    dmg_sha = checksum(args.dmg, "macOS DMG")
    win_sha = checksum(args.windows_installer, "Windows installer")
    if dmg_sha is None and win_sha is None:
        raise SystemExit("nothing to render: pass --dmg and/or --windows-installer, or --dry-run")

    values = {
        "VERSION": version,
        "REPO": repo,
        "DMG_SHA256": dmg_sha or "",
        "INSTALLER_URL": f"{base}/voxtype-{version}-windows-x64-setup.exe",
        "INSTALLER_SHA256": (win_sha or "").upper(),
        "RELEASE_NOTES_URL": f"https://github.com/{repo}/releases/tag/v{version}",
        "PACKAGE_ID": WINGET_ID,
    }

    if dmg_sha:
        cask = out / "homebrew-tap/Casks" / f"{CASK}.rb"
        cask.parent.mkdir(parents=True, exist_ok=True)
        cask.write_text(fill(CASK_TEMPLATE.read_text(), values))
        print(f"wrote {cask}")
    else:
        print("no macOS DMG: skipped the Homebrew cask")

    if win_sha:
        # winget-pkgs layout: manifests/<first letter>/<Publisher>/<Name>/<version>/
        publisher, name = WINGET_ID.split(".", 1)
        dest = out / "winget/manifests" / publisher[0].lower() / publisher / name / version
        dest.mkdir(parents=True, exist_ok=True)
        for tmpl in sorted(WINGET_TEMPLATES.glob("*.yaml.in")):
            target = dest / tmpl.name.removesuffix(".in").replace("PACKAGE_ID", WINGET_ID)
            target.write_text(fill(tmpl.read_text(), values))
            print(f"wrote {target}")
    else:
        print("no Windows installer: skipped the WinGet manifest")
    return 0


def load_schema(kind: str, version: str, cache: Path) -> dict:
    path = cache / f"manifest.{kind}.{version}.json"
    if not path.exists():
        url = SCHEMA_URL.format(commit=SCHEMA_COMMIT, ver=version, kind=kind)
        with urllib.request.urlopen(url, timeout=30) as resp:
            path.write_bytes(resp.read())
    return json.loads(path.read_text())


def validate_winget(manifests: list[Path], cache: Path) -> list[str]:
    import jsonschema
    import yaml

    errors: list[str] = []
    docs = {}
    for path in manifests:
        # WinGet reads YAML 1.2; safe_load is close enough for these files,
        # which quote every value that could be read as another type.
        doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        docs[path] = doc
        kind = {
            "version": "version",
            "installer": "installer",
            "defaultLocale": "defaultLocale",
            "locale": "locale",
            "singleton": "singleton",
        }.get(doc.get("ManifestType", ""))
        if kind is None:
            errors.append(f"{path}: unknown ManifestType {doc.get('ManifestType')!r}")
            continue
        schema = load_schema(kind, doc["ManifestVersion"], cache)
        validator = jsonschema.Draft7Validator(schema)
        for err in sorted(validator.iter_errors(doc), key=lambda e: list(e.path)):
            where = "/".join(str(p) for p in err.path) or "(root)"
            errors.append(f"{path.name}: {where}: {err.message}")
        if not path.read_text(encoding="utf-8").startswith("# yaml-language-server: $schema="):
            errors.append(f"{path.name}: missing the yaml-language-server schema header")

    # The multi-file manifest must agree on identity and version, and the
    # version manifest must name a default locale that exists.
    ids = {(d.get("PackageIdentifier"), d.get("PackageVersion"), d.get("ManifestVersion")) for d in docs.values()}
    if len(ids) > 1:
        errors.append(f"manifests disagree on identifier/version/schema: {sorted(map(str, ids))}")
    types = sorted(d.get("ManifestType") for d in docs.values())
    if types != ["defaultLocale", "installer", "version"]:
        errors.append(f"expected version + installer + defaultLocale manifests, got {types}")
    for path, d in docs.items():
        if d.get("ManifestType") == "version":
            locales = {x.get("PackageLocale") for x in docs.values() if x.get("ManifestType") == "defaultLocale"}
            if d.get("DefaultLocale") not in locales:
                errors.append(f"{path.name}: DefaultLocale {d.get('DefaultLocale')} has no defaultLocale manifest")
        expected = f"{d.get('PackageIdentifier')}" + {
            "version": ".yaml",
            "installer": ".installer.yaml",
            "defaultLocale": f".locale.{d.get('PackageLocale')}.yaml",
        }.get(d.get("ManifestType"), "")
        if path.name != expected:
            errors.append(f"{path.name}: winget-pkgs expects the file name {expected}")
    return errors


def validate_cask(cask: Path) -> list[str]:
    brew = shutil.which("brew")
    if brew:
        # brew only lints casks that live in a tap, so stage a throwaway one.
        repo = Path(subprocess.check_output([brew, "--repository"], text=True).strip())
        tap = repo / "Library/Taps" / TAP.split("/")[0] / f"homebrew-{TAP.split('/')[1]}-validate"
        if tap.exists():
            shutil.rmtree(tap)
        (tap / "Casks").mkdir(parents=True)
        shutil.copy(cask, tap / "Casks" / cask.name)
        env = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE="1", HOMEBREW_NO_ANALYTICS="1")
        name = f"{TAP.split('/')[0]}/{TAP.split('/')[1]}-validate/{cask.stem}"
        errors = []
        trusted = False
        try:
            # Homebrew refuses to load a cask from a tap that hasn't been
            # trusted ("Refusing to load cask ... from untrusted tap"). Trust
            # only this cask, only for the check; style and audit still run in
            # full. An older brew without `brew trust` has no such gate, and if
            # trust really was needed the audit below fails anyway.
            print(f"+ brew trust --cask {name}", flush=True)
            r = subprocess.run([brew, "trust", "--cask", name], env=env, capture_output=True, text=True)
            trusted = r.returncode == 0
            if not trusted:
                why = (r.stderr.strip().splitlines() or ["no output"])[-1]
                print(f"::warning::brew trust --cask {name} failed ({why}); this brew may predate tap trust")
            for cmd in (["style", "--cask", name], ["audit", "--cask", "--strict", name]):
                print("+ brew " + " ".join(cmd), flush=True)
                if subprocess.run([brew, *cmd], env=env).returncode != 0:
                    errors.append(f"brew {cmd[0]} failed on {cask}")
        finally:
            if trusted:
                subprocess.run([brew, "untrust", "--cask", name], env=env, stdout=subprocess.DEVNULL)
            shutil.rmtree(tap, ignore_errors=True)
        return errors
    ruby = shutil.which("ruby")
    if ruby:
        print("Homebrew not installed: syntax check only (ruby -c)")
        r = subprocess.run([ruby, "-c", str(cask)], capture_output=True, text=True)
        return [] if r.returncode == 0 else [f"ruby -c {cask}: {r.stderr.strip()}"]
    print("::warning::neither brew nor ruby is installed: the cask was not checked")
    return []


def validate(args: argparse.Namespace) -> int:
    root = Path(args.dir)
    errors: list[str] = []
    casks = sorted(root.glob("homebrew-tap/Casks/*.rb"))
    manifests_dirs = sorted({p.parent for p in root.glob("winget/manifests/**/*.yaml")})
    if not casks and not manifests_dirs:
        raise SystemExit(f"nothing to validate under {root}")
    cache = Path(args.schema_cache) if args.schema_cache else Path(tempfile.mkdtemp(prefix="winget-schemas-"))
    cache.mkdir(parents=True, exist_ok=True)
    for cask in casks:
        print(f"cask: {cask}")
        errors += validate_cask(cask)
    for d in manifests_dirs:
        print(f"winget: {d}")
        errors += validate_winget(sorted(d.glob("*.yaml")), cache)
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    print("OK" if not errors else f"{len(errors)} problem(s)")
    return 1 if errors else 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("render", help="fill the templates from release artifacts")
    r.add_argument("--version", required=True)
    r.add_argument("--repo", default=DEFAULT_REPO)
    r.add_argument("--dmg", help="the macOS DMG (voxtype-VERSION-macos-universal.dmg)")
    r.add_argument("--windows-installer", help="the Windows installer (voxtype-VERSION-windows-x64-setup.exe)")
    r.add_argument("--dry-run", action="store_true", help="zero checksums for missing artifacts")
    r.add_argument("--out", required=True)
    r.set_defaults(func=render)
    v = sub.add_parser("validate", help="check rendered manifests")
    v.add_argument("dir")
    v.add_argument("--schema-cache", help="directory to keep downloaded WinGet schemas in")
    v.set_defaults(func=validate)
    args = p.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
