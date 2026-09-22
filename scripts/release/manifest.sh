#!/usr/bin/env bash
# Build a release-channel manifest (see channel-manifest.schema.json) from a dist
# directory that holds the release assets and their SHA256SUMS.
#
#   manifest.sh --version V --tag T --channel <stable|alpha> --repo OWNER/NAME \
#               --dist DIR --out FILE [--asset-base URL] [--previous FILE] \
#               [--attested true|false] [--published-at RFC3339] [--force]
#
#   --asset-base  where the assets are downloadable; defaults to the GitHub release
#                 of --tag in --repo. Overridden by smoke tests to a loopback server.
#   --previous    the channel's current manifest. Refuses to move the channel to a
#                 lower version (downgrade protection) unless --force is given, and
#                 records the superseded version as previous_version/previous_tag so a
#                 rollback target is always one manifest away.
#
# Requires jq (present on GitHub-hosted runners).

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

version='' tag='' channel='' repo='' dist='' out='' asset_base='' previous='' attested=false published_at='' force=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --tag) tag=$2; shift 2 ;;
    --channel) channel=$2; shift 2 ;;
    --repo) repo=$2; shift 2 ;;
    --dist) dist=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --asset-base) asset_base=$2; shift 2 ;;
    --previous) previous=$2; shift 2 ;;
    --attested) attested=$2; shift 2 ;;
    --published-at) published_at=$2; shift 2 ;;
    --force) force=true; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done

need_cmd jq
is_semver "$version" || die "--version must be semver, got '$version'"
[[ "$tag" == "v$version" ]] || die "--tag must be v<version> (got '$tag' for $version)"
is_channel "$channel" || die "--channel must be stable or alpha"
is_repo_slug "$repo" || die "--repo must be OWNER/NAME, got '$repo'"
[[ -d "$dist" ]] || die "--dist directory not found: $dist"
[[ -f "$dist/SHA256SUMS" ]] || die "$dist/SHA256SUMS not found (run checksums.sh first)"
[[ -n "$out" ]] || die "--out is required"
[[ "$attested" == true || "$attested" == false ]] || die "--attested must be true or false"
asset_base=${asset_base:-$(release_download_base "$repo" "$tag")}
asset_base=${asset_base%/}
published_at=${published_at:-$(utc_now)}

prev_version='' prev_tag=''
if [[ -n "$previous" && -f "$previous" ]]; then
  cur_version=$(jq -r '.version // empty' "$previous")
  cur_tag=$(jq -r '.tag // empty' "$previous")
  if [[ -n "$cur_version" ]]; then
    is_semver "$cur_version" || die "previous manifest has a non-semver version: $cur_version"
    case "$(semver_cmp "$version" "$cur_version")" in
      -1)
        if $force; then
          log "warning: --force moving $channel from $cur_version down to $version"
        else
          die "refusing to move $channel channel from $cur_version down to $version (use --force for a deliberate rollback)"
        fi
        ;;
      0) log "republishing $channel at $version" ;;
      1) log "advancing $channel channel $cur_version -> $version" ;;
    esac
    if [[ "$cur_version" == "$version" ]]; then
      prev_version=$(jq -r '.previous_version // empty' "$previous")
      prev_tag=$(jq -r '.previous_tag // empty' "$previous")
    else
      prev_version=$cur_version
      prev_tag=$cur_tag
    fi
  fi
fi

artifacts='{}'
count=0
while read -r sha name; do
  [[ -n "$name" ]] || continue
  platform=${name#"$PRODUCT_BIN-$version-"}
  platform=${platform%.tar.gz}
  if [[ "$name" != "$PRODUCT_BIN-$version-"*.tar.gz ]] || ! is_platform "$platform"; then
    log "skipping non-asset entry in SHA256SUMS: $name"
    continue
  fi
  is_sha256 "$sha" || die "bad sha256 for $name: $sha"
  [[ -f "$dist/$name" ]] || die "listed asset missing from $dist: $name"
  binary="$PRODUCT_BIN"
  [[ "$platform" == windows-* ]] && binary="$PRODUCT_BIN.exe"
  artifacts=$(jq \
    --arg p "$platform" --arg url "$asset_base/$name" --arg sha "$sha" \
    --argjson size "$(file_size "$dist/$name")" --arg binary "$binary" \
    '. + {($p): {url: $url, sha256: $sha, size: $size, format: "tar.gz", binary: $binary}}' <<<"$artifacts")
  count=$((count + 1))
done <"$dist/SHA256SUMS"
((count > 0)) || die "no $PRODUCT_BIN-$version-<platform>.tar.gz assets listed in $dist/SHA256SUMS"

mkdir -p "$(dirname "$out")"
jq -n \
  --argjson schema_version "$MANIFEST_SCHEMA_VERSION" \
  --arg product "$PRODUCT_BIN" \
  --arg channel "$channel" \
  --arg version "$version" \
  --arg tag "$tag" \
  --arg published_at "$published_at" \
  --arg repo "$repo" \
  --arg release_url "$(release_url "$repo" "$tag")" \
  --arg checksums_url "$asset_base/SHA256SUMS" \
  --argjson attested "$attested" \
  --arg prev_version "$prev_version" \
  --arg prev_tag "$prev_tag" \
  --argjson artifacts "$artifacts" \
  '{
    schema_version: $schema_version,
    product: $product,
    channel: $channel,
    version: $version,
    tag: $tag,
    published_at: $published_at,
    release_repo: $repo,
    release_url: $release_url,
    checksums_url: $checksums_url,
    attested: $attested,
    previous_version: (if $prev_version == "" then null else $prev_version end),
    previous_tag: (if $prev_tag == "" then null else $prev_tag end),
    artifacts: $artifacts
  }' >"$out"
log "wrote $out ($channel -> $version, $count artifacts)"
