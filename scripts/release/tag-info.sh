#!/usr/bin/env bash
# Derive release metadata from a git tag. Prints `key=value` lines suitable for
# `>> "$GITHUB_OUTPUT"`.
#
#   tag-info.sh v1.2.3          -> version=1.2.3 tag=v1.2.3 channel=stable prerelease=false
#   tag-info.sh v1.2.3-alpha.1  -> version=1.2.3-alpha.1 ... channel=alpha prerelease=true
#
# Channel rule: any semver prerelease publishes to the alpha channel and is a
# GitHub pre-release; everything else is stable.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

tag=${1:-}
[[ -n "$tag" ]] || die "usage: tag-info.sh <tag>  (e.g. v1.2.3 or v1.2.3-alpha.1)"
[[ "$tag" == v* ]] || die "tag must start with 'v': $tag"
version=${tag#v}
is_semver "$version" || die "tag is not v<semver>: $tag"
[[ "$version" != *+* ]] || die "build metadata (+...) is not allowed in release tags: $tag"

channel=$(channel_for_version "$version")
prerelease=false
[[ "$channel" == alpha ]] && prerelease=true

printf 'version=%s\n' "$version"
printf 'tag=%s\n' "$tag"
printf 'channel=%s\n' "$channel"
printf 'prerelease=%s\n' "$prerelease"
