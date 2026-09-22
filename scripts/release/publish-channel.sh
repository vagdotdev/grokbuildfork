#!/usr/bin/env bash
# Update the `release-channel` branch of the release repo after a release:
# regenerate the channel manifest(s), publish the installer next to them, commit, push.
#
#   publish-channel.sh --version V --tag T --channel <stable|alpha> --repo OWNER/NAME \
#                      --dist DIR [--remote URL|PATH] [--attested true|false] \
#                      [--install-sh PATH] [--force] [--dry-run]
#
# Branch contents (served at https://raw.githubusercontent.com/OWNER/NAME/release-channel/):
#   stable.json, alpha.json            channel manifests (channel-manifest.schema.json)
#   install.sh                         scripts/install.sh with the release repo stamped in
#   channel-manifest.schema.json, README.md
#
# Channel policy:
#   alpha release  -> alpha.json
#   stable release -> stable.json, and alpha.json unless alpha is already ahead
#   A channel never moves backwards without --force (see manifest.sh).
#
# Auth: GH_TOKEN (contents:write on --repo) is passed through a git credential helper
# so it never appears in the remote URL or process list. --remote may be a local path
# (bare repo) for tests.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

version='' tag='' channel='' repo='' dist='' remote='' attested=false install_sh='' force=false dry_run=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --tag) tag=$2; shift 2 ;;
    --channel) channel=$2; shift 2 ;;
    --repo) repo=$2; shift 2 ;;
    --dist) dist=$2; shift 2 ;;
    --remote) remote=$2; shift 2 ;;
    --attested) attested=$2; shift 2 ;;
    --install-sh) install_sh=$2; shift 2 ;;
    --force) force=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done
is_semver "$version" || die "--version must be semver"
[[ "$tag" == "v$version" ]] || die "--tag must be v<version>"
is_channel "$channel" || die "--channel must be stable or alpha"
is_repo_slug "$repo" || die "--repo must be OWNER/NAME"
[[ -f "$dist/SHA256SUMS" ]] || die "$dist/SHA256SUMS not found"
need_cmd git
need_cmd jq
install_sh=${install_sh:-$here/../install.sh}
[[ -f "$install_sh" ]] || die "install.sh not found: $install_sh"
remote=${remote:-https://github.com/$repo.git}

git_auth=()
if [[ -n "${GH_TOKEN:-}" && "$remote" == https://* ]]; then
  # $GH_TOKEN is expanded by git's helper shell at use time, not here.
  # shellcheck disable=SC2016
  git_auth=(-c credential.helper= -c 'credential.helper=!f() { echo "username=x-access-token"; echo "password=$GH_TOKEN"; }; f')
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git init -q "$work"
cd "$work" || die "cannot enter $work"
git remote add origin "$remote"
if git "${git_auth[@]}" fetch -q origin "$CHANNEL_BRANCH" 2>/dev/null; then
  git checkout -q -b "$CHANNEL_BRANCH" FETCH_HEAD
  log "updating existing $CHANNEL_BRANCH branch ($(git rev-parse --short HEAD))"
else
  git checkout -q --orphan "$CHANNEL_BRANCH"
  log "creating $CHANNEL_BRANCH branch (first release)"
fi

channels=("$channel")
if [[ "$channel" == stable ]]; then
  if [[ -f alpha.json ]] && alpha_version=$(jq -r '.version // empty' alpha.json) && [[ -n "$alpha_version" ]] \
    && [[ "$(semver_cmp "$version" "$alpha_version")" == -1 ]]; then
    log "alpha channel ($alpha_version) is ahead of $version; leaving alpha.json untouched"
  else
    channels+=(alpha)
  fi
fi

force_flag=()
$force && force_flag=(--force)
for ch in "${channels[@]}"; do
  prev=''
  if [[ -f "$ch.json" ]]; then
    prev="$work/.prev-$ch.json"
    cp "$ch.json" "$prev"
  fi
  "$here/manifest.sh" --version "$version" --tag "$tag" --channel "$ch" --repo "$repo" \
    --dist "$dist" --out "$work/$ch.json" --attested "$attested" \
    ${prev:+--previous "$prev"} "${force_flag[@]}"
  rm -f "$prev"
done

sed "s|^WORKSHOP_RELEASE_REPO_DEFAULT=.*|WORKSHOP_RELEASE_REPO_DEFAULT=\"$repo\"|" "$install_sh" >install.sh
grep -q "^WORKSHOP_RELEASE_REPO_DEFAULT=\"$repo\"$" install.sh || die "failed to stamp release repo into install.sh"
chmod 755 install.sh
cp "$here/channel-manifest.schema.json" channel-manifest.schema.json

raw_base=$(channel_raw_base "$repo")
cat >README.md <<EOF
# Workshop release channel

Machine-readable release pointers, updated by the release workflow. Do not edit by hand;
to roll a channel back, revert the commit that moved it.

| File | Purpose |
|---|---|
| \`stable.json\`, \`alpha.json\` | Channel manifests: version, per-platform asset URLs and SHA-256 (\`channel-manifest.schema.json\`) |
| \`install.sh\` | Installer: \`curl -fsSL $raw_base/install.sh \| sh\` (\`WORKSHOP_CHANNEL=alpha\` for alpha) |

Releases: https://github.com/$repo/releases
EOF

git add -A
if git diff --cached --quiet; then
  log "channel branch already up to date; nothing to commit"
  exit 0
fi
git -c user.name='workshop-release[bot]' -c user.email='workshop-release[bot]@users.noreply.github.com' \
  commit -q -m "release: $tag -> ${channels[*]}" -m "Release: $(release_url "$repo" "$tag")"
git show --stat --format='%h %s' HEAD >&2

if $dry_run; then
  log "dry run: not pushing to $remote"
  exit 0
fi

push() { git "${git_auth[@]}" push -q origin "HEAD:refs/heads/$CHANNEL_BRANCH"; }
if ! push; then
  log "push rejected; rebasing onto the current $CHANNEL_BRANCH and retrying once"
  git "${git_auth[@]}" fetch -q origin "$CHANNEL_BRANCH"
  git rebase -q FETCH_HEAD || die "rebase conflict on $CHANNEL_BRANCH; re-run the channel job after the concurrent release finishes"
  push
fi
log "pushed $CHANNEL_BRANCH to $repo: $raw_base/${channels[0]}.json"
