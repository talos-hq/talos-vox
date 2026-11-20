# Homebrew cask and WinGet manifest (M5c)

Templates for the two package managers that update macOS and Windows
installs for us:

| File | Becomes |
|---|---|
| `packaging/homebrew-tap/Casks/voxtype.rb.in` | `Casks/voxtype.rb` in [talos-hq/homebrew-tap](https://github.com/talos-hq/homebrew-tap), installed with `brew install --cask talos-hq/tap/voxtype` (from the first tagged release) |
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
4. On a stable tag (`vX.Y.Z`, no `-` suffix) it publishes the cask: see
   [Publishing](#publishing) below.

Validation, on the Ubuntu runner:

- Cask: `brew style --cask` and `brew audit --cask --strict` against a
  throwaway local tap. Current Homebrew won't load a cask from an untrusted
  tap, so the script runs `brew trust --cask` on that one cask for the check
  and `brew untrust --cask` afterwards. With no Homebrew installed the script falls back to
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

## Publishing

### Homebrew: automatic on every stable tag

```sh
brew install --cask talos-hq/tap/voxtype
```

This works from the first tagged talos-vox release. Before that the tap has no
`Casks/voxtype.rb`, so the command fails.

The last step of the `package-manifests` job, "Publish the cask to
talos-hq/homebrew-tap", runs only on a `push` of a `v*` tag without a `-`.
Prerelease tags (`v1.2.0-rc1`) build, render and validate as usual but are never
published. The step runs after the validation above has passed, and then:

1. Checks that the release is not a draft and that the DMG attached to it has
   the SHA-256 in the rendered cask (a re-run can replace the asset).
2. Clones talos-hq/homebrew-tap over SSH with the write deploy key in the
   `HOMEBREW_TAP_DEPLOY_KEY` repo secret, pinning github.com's host key.
   GITHUB_TOKEN can't push to another repository.
3. Copies the cask to `Casks/voxtype.rb` and commits it as `voxtype <version>`.
   If the cask is unchanged, as on a re-run, it pushes nothing.
4. Pushes to `main`. The clud release workflow also pushes to that tap (it owns
   `Formula/clud.rb`), so a rejected push refetches `main` and recreates the
   one-file commit, up to three times.

If the secret is missing, the release is a draft, or the checksums disagree,
the step fails and so does `macos-ci-success`. The rendered cask is still
uploaded as an artifact first, so it can be copied into the tap by hand:

```bash
gh run download <run-id> --repo talos-hq/talos-vox \
  --name voxtype-<version>-package-manifests --dir package-manifests
cp package-manifests/homebrew-tap/Casks/voxtype.rb <homebrew-tap>/Casks/
```

To rotate the key, generate a new ed25519 keypair, replace the write deploy
key on talos-hq/homebrew-tap, and pipe the private key into
`gh secret set HOMEBREW_TAP_DEPLOY_KEY --repo talos-hq/talos-vox`.

### WinGet: not published

The WinGet manifest is rendered and validated but only uploaded as an
artifact. Nothing opens a microsoft/winget-pkgs PR, and that needs separate
owner approval. To submit one by hand from the artifact above:

```bash
cp -r package-manifests/winget/manifests/t <winget-pkgs>/manifests/
winget validate --manifest <winget-pkgs>/manifests/t/TalosHQ/Voxtype/<version>   # on Windows
```

homebrew-core is not a target: it requires notable usage and builds from
source, and a tap can ship the notarized app directly.
