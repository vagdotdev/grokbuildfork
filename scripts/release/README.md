# Workshop distribution pipeline

Tag-triggered releases of the `workshop` binary, a `release-channel` manifest the
installer and the in-app updater read, and a `curl | sh` installer. Everything here
is Workshop-owned: no xAI CDN, bucket, or npm package is referenced anywhere.

## One knob: `WORKSHOP_RELEASE_REPO`

Releases live in one GitHub repo, named in exactly one place per component:

| Component | Where the repo comes from |
|---|---|
| `.github/workflows/release.yml` | repo **variable** `WORKSHOP_RELEASE_REPO` (Settings → Secrets and variables → Actions → Variables); defaults to the repo running the workflow |
| `scripts/install.sh` | `WORKSHOP_RELEASE_REPO` env var, else `WORKSHOP_RELEASE_REPO_DEFAULT` (stamped by `publish-channel.sh` when the installer is published to the branch) |
| channel manifest | `release_repo` field, written by `manifest.sh` |
| in-app updater | build-time `WORKSHOP_RELEASE_REPO` (see the updater repoint spec in the project store) |

Today the default is `vagdotdev/grokbuildfork`. When releases move to the public repo,
set the variable there (or run this workflow from that repo) and nothing else changes.
Cross-repo publishing additionally needs the secret `WORKSHOP_RELEASE_TOKEN`
(`contents: write` on the release repo).

## Cutting a release

```sh
git tag v0.1.0            # stable channel
git tag v0.1.0-alpha.1    # alpha channel + GitHub pre-release
git push origin <tag>
```

Any semver prerelease goes to alpha. Tags must be `v<semver>` without build metadata.
The version is stamped into the binary at build time (`GROK_VERSION`, later
`WORKSHOP_VERSION`), so `workshop --version` and the updater compare against exactly
the tag version.

Pipeline (`release.yml`):

1. `meta` — `tag-info.sh` derives version/channel.
2. `lint` — shellcheck, forbidden-URL scan, schema is valid JSON.
3. `build` — matrix: `linux-x86_64` (ubuntu-22.04 for wide glibc), `linux-aarch64`
   (ubuntu-22.04-arm), `macos-aarch64` (macos-15), `macos-x86_64` (macos-15-intel),
   `windows-x86_64` (best-effort, `continue-on-error`). `cargo build --locked --profile
   release-dist -p xai-grok-pager-bin`, stripped via `CARGO_PROFILE_RELEASE_DIST_STRIP`.
   `package.sh` stages the binary as `workshop` (falls back to upstream's
   `xai-grok-pager` name until the branding patch renames the `[[bin]]`).
4. `checksums` — `SHA256SUMS`; GitHub artifact attestations (`actions/attest`) when the
   repo is public. Private repos cannot attest without Enterprise Cloud, so the step is
   skipped and `attested: false` is recorded in the manifest and release notes.
5. `smoke` — `smoke-install.sh` on ubuntu + macos-15: serves the freshly built assets
   from a loopback HTTP server and runs the real `install.sh` through manifest → download
   → checksum → install → `workshop --version`, plus tampered-checksum and non-https
   rejection cases.
6. `publish` — `release-notes.sh` + `publish-release.sh` (`gh release create`, idempotent).
7. `channel` — `publish-channel.sh` regenerates the manifest(s) and pushes the
   `release-channel` branch.

`workflow_dispatch` runs steps 1–5 as a dry run and publishes nothing.

Build prerequisites on runners: a C/C++ toolchain, `cmake` (aws-lc-sys), `protoc` 29.3
(installed by the workflow to match `bin/protoc`'s DotSlash pin), NASM on Windows.

## Channel manifest

Branch `release-channel` of the release repo, served from
`https://raw.githubusercontent.com/<repo>/release-channel/`:

| File | Purpose |
|---|---|
| `stable.json`, `alpha.json` | one manifest per channel (`channel-manifest.schema.json`) |
| `install.sh` | the installer, repo stamped in |
| `channel-manifest.schema.json`, `README.md` | format + orientation |

```json
{
  "schema_version": 1,
  "product": "workshop",
  "channel": "stable",
  "version": "0.1.0",
  "tag": "v0.1.0",
  "published_at": "2026-09-21T20:27:58Z",
  "release_repo": "vagdotdev/grokbuildfork",
  "release_url": "https://github.com/vagdotdev/grokbuildfork/releases/tag/v0.1.0",
  "checksums_url": "https://github.com/vagdotdev/grokbuildfork/releases/download/v0.1.0/SHA256SUMS",
  "attested": false,
  "previous_version": null,
  "previous_tag": null,
  "artifacts": {
    "linux-x86_64": {
      "url": "https://github.com/vagdotdev/grokbuildfork/releases/download/v0.1.0/workshop-0.1.0-linux-x86_64.tar.gz",
      "sha256": "69dc9953e13053aeab831a3c8961cc31e3aea1279e15a8839bf2f37b6a8d6df7",
      "size": 84543,
      "format": "tar.gz",
      "binary": "workshop"
    }
  }
}
```

Artifact keys are `<os>-<arch>` with `os ∈ {linux, macos, windows}` and
`arch ∈ {x86_64, aarch64}` — the same label the updater's `detect_platform()` produces,
so it can index the map directly. Archives are flat: `workshop` (or `workshop.exe`),
`LICENSE`, `THIRD-PARTY-NOTICES`.

Channel policy (`publish-channel.sh` / `manifest.sh`):

- alpha release → `alpha.json`; stable release → `stable.json` and `alpha.json` unless
  alpha is already ahead.
- A channel never moves to a lower version without `--force`; the superseded version is
  kept as `previous_version` / `previous_tag`.
- Rollback: `git revert` the manifest commit on `release-channel`, or reinstall with
  `WORKSHOP_VERSION=<previous>`. Old versions stay in `~/.workshop/downloads/`.

## Installer

```sh
curl -fsSL https://raw.githubusercontent.com/vagdotdev/grokbuildfork/release-channel/install.sh | sh
```

`scripts/install.sh` is POSIX sh. It detects OS/arch (Rosetta → arm64), reads the
channel manifest, downloads the archive, verifies SHA-256, installs
`$WORKSHOP_HOME/downloads/workshop-<version>-<platform>` and atomically points
`$WORKSHOP_HOME/bin/workshop` at it (the same managed layout the updater uses, so it can
read the installed version from the symlink and keep N-1 for rollback), then runs
`workshop --version` and prints a PATH hint. Two network requests, no telemetry, https
only (loopback http is accepted for tests).

Environment: `WORKSHOP_CHANNEL` (`stable`|`alpha`), `WORKSHOP_VERSION` (pin; verifies
against that release's `SHA256SUMS`), `WORKSHOP_HOME` (default `~/.workshop`),
`WORKSHOP_RELEASE_REPO`, `WORKSHOP_MANIFEST_URL`, `WORKSHOP_DOWNLOAD_BASE`.

### macOS: unsigned, not notarized

Workshop binaries carry only an ad-hoc signature (`codesign -s -`), no Apple Developer
ID, no notarization (decision). `install.sh` runs `xattr -d com.apple.quarantine` on the
installed binary and prints the manual fallback. If macOS ever blocks it:

```sh
xattr -d com.apple.quarantine ~/.workshop/bin/workshop
```

Release notes carry the same statement (`release-notes.sh`). The root README is
upstream-owned; the branding pass should add this section there.

## Local dry run

```sh
# any binary works as a stand-in; the real one comes from
#   PROTOC=... GROK_VERSION=0.1.0 cargo build --release -p xai-grok-pager-bin
scripts/release/package.sh --version 0.1.0 --platform linux-x86_64 --target-dir target/release --out dist
scripts/release/checksums.sh dist
scripts/release/smoke-install.sh --dist dist --version 0.1.0 --channel stable

# channel branch against a throwaway bare repo
git init --bare /tmp/remote.git
scripts/release/publish-channel.sh --version 0.1.0 --tag v0.1.0 --channel stable \
  --repo vagdotdev/grokbuildfork --dist dist --remote /tmp/remote.git

shellcheck -s sh scripts/install.sh && shellcheck -x scripts/release/*.sh
```

Note: `actionlint` ≤ 1.7.7 does not know the `macos-15-intel` label yet; GitHub's runner
docs list it for public and private repos.

## Deliberately not here (see the Workshop roadmap in the project store)

Apple Developer ID signing/notarization, npm package, Homebrew tap, Windows installer and
code signing, manifest signing for offline updater verification, enabling auto-update.
