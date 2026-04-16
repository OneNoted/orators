# AUR packaging

This repository tracks two AUR packages:

- `orators-bin`: prebuilt `x86_64` binaries from GitHub tagged releases
- `orators-git`: latest `main` built directly from GitHub

## Package layout

- `packaging/systemd/user/oratorsd.service` is the packaged user unit installed by both AUR variants.
- `packaging/aur/orators-git/` is committed in the final AUR-ready form.
- `packaging/aur/orators-bin/PKGBUILD.in` is a template. The release/AUR automation renders it with the exact GitHub release asset URL and SHA-256 before pushing to AUR.

## Why `bluez-alsa-git`

As of April 16, 2026, the AUR RPC search for `bluez-alsa` returns `bluez-alsa-git`, so both Orators packages currently depend on that package for the BlueALSA runtime.

## GitHub Actions secrets

Set these repository secrets before enabling automatic AUR publishing:

- `AUR_SSH_PRIVATE_KEY`: SSH private key for the AUR account that maintains `orators-bin` and `orators-git`
- `AUR_PACKAGER_NAME`: optional override for the git commit author name; defaults to `Jonatan Jonasson`
- `AUR_PACKAGER_EMAIL`: optional override for the git commit author email; defaults to `notes@madeingotland.com`

## Release flow

1. Bump the workspace version in `Cargo.toml`.
2. Create and push a tag like `v0.1.0`.
3. `.github/workflows/release.yml` builds the release archive and publishes it to GitHub Releases.
4. `.github/workflows/aur-bin.yml` renders the `orators-bin` PKGBUILD with the exact release URL and checksum, then pushes it to AUR.
5. `scripts/aur/publish_aur_package.sh` bootstraps the initial AUR git push to `master` if the package repo does not exist yet.

## Updating `orators-git`

The `orators-git` package does not need per-commit updates. Push its AUR repo only when packaging metadata changes, such as dependencies, install messaging, or service file packaging.

## Local validation

```bash
./scripts/release/build-release-archive.sh
./scripts/aur/render_orators_bin_pkgbuild.py \
  --version 0.1.0 \
  --source-url https://example.invalid/orators-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --output-dir /tmp/orators-bin
./scripts/aur/generate_srcinfo.sh /tmp/orators-bin/PKGBUILD > /tmp/orators-bin/.SRCINFO
./scripts/aur/generate_srcinfo.sh packaging/aur/orators-git/PKGBUILD > /tmp/orators-git.SRCINFO
```
