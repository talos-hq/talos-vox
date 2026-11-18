# Self-updater (M7): deferred, with the design it should follow

Status: **not implemented**. This note records why, what has to exist
first, and the shape it should take, so the work can start when those
prerequisites land. Plan context: `docs/HEX-ABSORB-PLAN.md` §4.5 and #3 M7.

## Why not now

An updater is only as trustworthy as the thing it verifies. Today there is
nothing it could verify that an attacker with write access to a GitHub
release could not also forge:

| Requirement | State on talos-hq/talos-vox |
|---|---|
| Releases to check | None published yet |
| macOS: Developer ID + notarization | Signing wired (#18); notarization blocked on the notary app-specific password (#23 item 4) |
| Windows: Authenticode | Blocked on the federated credential for Azure Artifact Signing (#23 item 5); no Windows release build yet |
| Linux: a detached signature | `build-linux.yml` GPG-signs `SHA256SUMS.txt`, but with upstream's CI key; this repo has no `GPG_PRIVATE_KEY` secret |
| An updater signing key | None. An ed25519 key pair has to be generated and the private half stored as a secret by the owner |

A checksum file downloaded from the same release as the binary only proves
the download was not corrupted. It does not prove who built it. Shipping a
download-and-replace path on that basis is trust-on-first-use with extra
steps, which the plan already rules out ("An updater before notarization or
Authenticode can only be trust-on-first-use, so it is pointless").

Meanwhile, once published, the M5c cask and WinGet manifest (#23 item 6,
`packaging/homebrew-tap/`) give macOS and Windows users package-manager
updates without any code in voxtype.

`voxtype check-update` (`src/app/updates.rs`) only reports a newer
release, and it asks upstream's `peteonrails/voxtype`, not this fork.
Pointing it at `talos-hq/talos-vox` is a fork-only divergence (§4 of the
plan), so it waits until this fork publishes releases of its own.

## Design, for when the prerequisites exist

**Opt-in, never on by default.** `[updates] auto_check = false`. When it is
false, nothing touches the network except an explicit
`voxtype check-update`. Applying an update is always an explicit command
(`voxtype update`) or a click in `voxtype-app`, never silent.

**Never on package-managed installs.** Refuse, and print the package
manager's command, when the binary is under `/usr`, `/nix/store`, a Homebrew
prefix or Cellar, a WinGet-managed install, or any install that has a
`packaged` marker file written by deb/rpm/AUR/Nix. On Linux only a
user-owned AppImage or tarball install updates itself.

**Check.** `GET https://api.github.com/repos/talos-hq/talos-vox/releases/latest`
(stable channel; RCs only with `channel = "rc"`). Compare semver, and pick the
asset for this OS/arch by exact file name.

**Verify, in this order, and abort on the first failure:**

1. **Manifest signature.** The release carries `update-manifest.json` with
   `{version, assets: [{name, sha256, size}]}` plus
   `update-manifest.json.sig`, an ed25519 signature made in the release job by
   a key used for nothing else. The public key is compiled into the binary
   (`ed25519-dalek`, already a small, audited crate). Key rotation: the
   binary may embed two public keys, current and next.
2. **Rollback protection.** The manifest version must be greater than the
   running version.
3. **Asset hash and size** match the signed manifest. Download to a temp
   file in the same directory as the target, so the final rename is atomic.
4. **Platform signature, as a second, independent check:**
   - macOS: `SecStaticCodeCheckValidity` with a requirement pinning our team
     ID (`anchor apple generic and certificate leaf[subject.OU] = "<team>"`),
     plus `spctl --assess` for notarization. Then swap the whole `.app` with
     `renamex_np(RENAME_SWAP)`. Never patch a binary inside a signed bundle.
   - Windows: `WinVerifyTrust` (Authenticode, with revocation checks), and
     the signer's subject matched to our Artifact Signing publisher. Run the
     signed per-user installer silently rather than replacing a running exe.
   - Linux AppImage: the ed25519 manifest check is the signature.

**Apply and roll back.** Keep the previous version beside the new one. If the
new binary fails `--version` after the swap, restore the old one. The daemon
is restarted through its supervisor (systemd user unit, launch agent or
`voxtype-app`), never by the updater killing it mid-dictation. Wait for the
state to be `idle`.

**Size.** About 400-600 lines plus `ed25519-dalek`. The platform checks need
`Security.framework` bindings on macOS and `WinVerifyTrust` from
`windows-sys` (already a dependency) on Windows.

## Prerequisites checklist (owner)

- [ ] Notarized macOS release (#23 item 4)
- [ ] Authenticode-signed Windows installer (#23 item 5)
- [ ] An ed25519 updater key: generate it offline, store the private key as
      a `release`-environment secret, and commit the public key
- [ ] A first published talos-vox release with `update-manifest.json`
