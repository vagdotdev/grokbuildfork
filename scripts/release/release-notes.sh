#!/usr/bin/env bash
# Print the GitHub Release body (Markdown) for a Workshop release.
#
#   release-notes.sh --version V --tag T --channel <stable|alpha> --repo OWNER/NAME \
#                    --dist DIR [--attested true|false]
#
# `gh release create --generate-notes` appends the auto-generated changelog after this.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

version='' tag='' channel='' repo='' dist='' attested=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --tag) tag=$2; shift 2 ;;
    --channel) channel=$2; shift 2 ;;
    --repo) repo=$2; shift 2 ;;
    --dist) dist=$2; shift 2 ;;
    --attested) attested=$2; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
is_semver "$version" || die "--version must be semver"
[[ "$tag" == "v$version" ]] || die "--tag must be v<version>"
is_channel "$channel" || die "--channel must be stable or alpha"
is_repo_slug "$repo" || die "--repo must be OWNER/NAME"
[[ -f "$dist/SHA256SUMS" ]] || die "$dist/SHA256SUMS not found"

raw_base=$(channel_raw_base "$repo")
download_base=$(release_download_base "$repo" "$tag")

channel_env=''
[[ "$channel" == alpha ]] && channel_env="WORKSHOP_CHANNEL=alpha "

cat <<EOF
# Workshop $version ($channel channel)

## Install

\`\`\`sh
curl -fsSL $raw_base/install.sh | ${channel_env}sh
\`\`\`

Installs to \`~/.workshop/bin/workshop\` (override with \`WORKSHOP_HOME\`). Pin this exact version with \`WORKSHOP_VERSION=$version\`. The installer downloads the archive for your platform, verifies its SHA-256 against the channel manifest, and makes no other network requests. No telemetry.

## macOS: unsigned binary

Workshop is **not** signed with an Apple Developer ID and is not notarized. The installer removes the quarantine attribute after download, so \`workshop\` runs from the terminal. If macOS still refuses to start it ("cannot be opened because the developer cannot be verified"), for example after unpacking an archive in Finder, run this once; it also covers the voice helper and the OpenCode copy Workshop installs beside it:

\`\`\`sh
xattr -dr com.apple.quarantine ~/.workshop
\`\`\`

## Assets

| Platform | File | SHA-256 |
|---|---|---|
EOF

while read -r sha name; do
  [[ -n "$name" ]] || continue
  platform=${name#"$PRODUCT_BIN-$version-"}
  platform=${platform%.tar.gz}
  is_platform "$platform" || continue
  # shellcheck disable=SC2016  # backticks are Markdown, not command substitution
  printf '| `%s` | [%s](%s/%s) | `%s` |\n' "$platform" "$name" "$download_base" "$name" "$sha"
done <"$dist/SHA256SUMS"

cat <<EOF

Voice dictation assets (installed by the same \`install.sh\` run; see \`voice/MODEL.lock.json\`):

| File | SHA-256 |
|---|---|
EOF
while read -r sha name; do
  [[ -n "$name" ]] || continue
  case "$name" in
    voice-engine-*.tar.gz | ggml-*.bin | MODEL.lock.json)
      # shellcheck disable=SC2016
      printf '| [%s](%s/%s) | `%s` |\n' "$name" "$download_base" "$name" "$sha"
      ;;
  esac
done <"$dist/SHA256SUMS"

cat <<EOF

Windows assets, when present, are best-effort and unsupported.

## Verify

\`\`\`sh
curl -fsSLO $download_base/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing   # macOS: shasum -a 256 -c SHA256SUMS
\`\`\`
EOF

if [[ "$attested" == true ]]; then
  cat <<EOF

Build provenance (SLSA, Sigstore-signed via GitHub artifact attestations):

\`\`\`sh
gh attestation verify $(asset_name "$version" "<platform>") --repo $repo
\`\`\`
EOF
else
  cat <<EOF

Build provenance attestations are not generated while the release repository is private (GitHub limits artifact attestations to public repos on non-Enterprise plans). Rely on SHA256SUMS for this release.
EOF
fi

cat <<EOF

## Channel manifest

\`$raw_base/$channel.json\` now points at $version. Rollback: reinstall the previous version with \`WORKSHOP_VERSION=<previous> curl -fsSL $raw_base/install.sh | sh\` or revert the manifest commit on the \`$CHANNEL_BRANCH\` branch.
EOF
