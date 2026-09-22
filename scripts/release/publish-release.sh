#!/usr/bin/env bash
# Create (or refresh) the GitHub Release for a tag and upload the dist assets.
# Idempotent: re-running uploads assets with --clobber instead of failing.
#
#   publish-release.sh --tag T --version V --channel C --repo OWNER/NAME --dist DIR \
#                      --notes FILE [--source-repo OWNER/NAME] [--dry-run]
#
#   --repo         release repo (WORKSHOP_RELEASE_REPO). When it differs from
#                  --source-repo (the repo running the workflow) the tag is created
#                  by GitHub on that repo's default branch, so the release is a pure
#                  artifact drop there. Needs GH_TOKEN with contents:write on --repo
#                  (secret WORKSHOP_RELEASE_TOKEN in the workflow).
#   --skip-upload  the assets are already on the release (the workflow's build jobs
#                  upload straight to a draft); only set the notes/title and publish.
#   --dry-run      print the gh commands instead of running them.
#
# An existing release (the workflow's draft, or a re-run) is refreshed in place and
# taken out of draft; a missing one is created published.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

tag='' version='' channel='' repo='' dist='' notes='' source_repo='' skip_upload=false dry_run=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) tag=$2; shift 2 ;;
    --version) version=$2; shift 2 ;;
    --channel) channel=$2; shift 2 ;;
    --repo) repo=$2; shift 2 ;;
    --dist) dist=$2; shift 2 ;;
    --notes) notes=$2; shift 2 ;;
    --source-repo) source_repo=$2; shift 2 ;;
    --skip-upload) skip_upload=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done
is_semver "$version" || die "--version must be semver"
[[ "$tag" == "v$version" ]] || die "--tag must be v<version>"
is_channel "$channel" || die "--channel must be stable or alpha"
is_repo_slug "$repo" || die "--repo must be OWNER/NAME"
[[ -f "$dist/SHA256SUMS" ]] || die "$dist/SHA256SUMS not found"
[[ -f "$notes" ]] || die "--notes file not found: $notes"
need_cmd gh

shopt -s nullglob
# CLI archives, the voice helper archives, the Whisper model mirror files and their lock, then SHA256SUMS.
assets=("$dist"/"$PRODUCT_BIN"-*.tar.gz "$dist"/voice-engine-*.tar.gz "$dist"/ggml-*.bin "$dist"/MODEL.lock.json "$dist"/SHA256SUMS)
((${#assets[@]} > 1)) || die "no assets to upload in $dist"

run() {
  if $dry_run; then
    printf '+'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

if ! $dry_run && gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
  log "release $tag already exists on $repo; refreshing notes and publishing"
  $skip_upload || run gh release upload "$tag" "${assets[@]}" --repo "$repo" --clobber
  edit=(gh release edit "$tag" --repo "$repo" --title "Workshop $version" --notes-file "$notes" --draft=false)
  if [[ "$channel" == alpha ]]; then edit+=(--prerelease); else edit+=(--prerelease=false); fi
  run "${edit[@]}"
  log "published $(release_url "$repo" "$tag")"
  exit 0
fi

create=(gh release create "$tag" "${assets[@]}" --repo "$repo" --title "Workshop $version" --notes-file "$notes" --generate-notes)
[[ "$channel" == alpha ]] && create+=(--prerelease)
if [[ -n "$source_repo" && "$source_repo" == "$repo" ]]; then
  create+=(--verify-tag)
elif [[ -n "$source_repo" ]]; then
  log "release repo $repo differs from source repo $source_repo; tag $tag will be created on $repo's default branch"
fi
run "${create[@]}"
if $dry_run; then
  log "dry run: would publish $(release_url "$repo" "$tag")"
else
  log "published $(release_url "$repo" "$tag")"
fi
