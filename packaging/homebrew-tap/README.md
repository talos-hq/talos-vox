# Homebrew cask and WinGet manifest (M5c)

Templates for the two package managers that update macOS and Windows
installs for us:

| File | Becomes |
|---|---|
| `packaging/homebrew-tap/Casks/voxtype.rb.in` | `Casks/voxtype.rb` in [talos-hq/homebrew-tap](https://github.com/talos-hq/homebrew-tap), installed with `brew install --cask talos-hq/tap/voxtype` |
| `packaging/winget/*.yaml.in` | `manifests/t/TalosHQ/Voxtype/<version>/` in a [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) PR, installed with `winget install TalosHQ.Voxtype` |

`@NAME@` placeholders are filled by `scripts/package-manifests.py`, never by
hand. The old `packaging/homebrew/` formula and cask are upstream's
(peteonrails/voxtype) and are left alone.

## How a release fills them in

The `package-manifests` job in `.github/workflows/build-macos.yml` runs after
the macOS build on every run of that workflow:

1. It renders the templates with zero checksums and validates them (a dry
   run, so a broken template fails before any release depends on it).
2. It renders them again from the real artifacts: the DMG this run built, and
   on a tag, `voxtype-<version>-windows-x64-setup.exe` from the release if
   the Windows job (M5b) has uploaded one. Without an installer only the cask
   is rendered.
3. It validates the result and uploads the rendered tree as the
   `voxtype-<version>-package-manifests` workflow artifact.

Validation, on the Ubuntu runner:

- Cask: `brew style --cask` and `brew audit --cask --strict` against a
  throwaway local tap. With no Homebrew installed the script falls back to
  `ruby -c`, then to a warning.
- WinGet: each YAML file against Microsoft's JSON schema for its
  `ManifestVersion` (from `microsoft/winget-cli`, pinned to a commit in the
  script), plus the cross-file rules winget-pkgs enforces (same identifier
  and version, a default-locale file, winget-pkgs file names).
  `winget validate` itself only runs on Windows.

Run it locally:

```bash
uv run --with pyyaml --with jsonschema python scripts/package-manifests.py \
  render --version 1.1.1 --dry-run --out /tmp/manifests
uv run --with pyyaml --with jsonschema python scripts/package-manifests.py \
  validate /tmp/manifests
```

## Publishing: owner only, and not yet

Nothing in CI pushes to the tap or opens a winget-pkgs PR. Both wait on:

- **Signed artifacts.** The cask should point at a notarized DMG (blocked on
  the notary app-specific password, #23 item 4); WinGet's validation pipeline
  rejects unsigned installers in practice and SmartScreen warns on them (#23
  item 5, Azure Artifact Signing).
- **The Windows installer itself.** There is no Windows release build yet,
  so the WinGet manifest has no artifact to point at.
- **Owner approval** to publish under the talos-hq name.

Then, per release:

```bash
gh run download <run-id> --repo talos-hq/talos-vox \
  --name voxtype-<version>-package-manifests --dir package-manifests
# Homebrew: copy into the existing tap repo.
cp package-manifests/homebrew-tap/Casks/voxtype.rb <homebrew-tap>/Casks/
# WinGet: open a PR on a winget-pkgs fork.
cp -r package-manifests/winget/manifests/t <winget-pkgs>/manifests/
winget validate --manifest <winget-pkgs>/manifests/t/TalosHQ/Voxtype/<version>   # on Windows
```

homebrew-core is not a target: it requires notable usage and builds from
source, and a tap can ship the notarized app directly.
