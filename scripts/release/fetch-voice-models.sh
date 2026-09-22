#!/usr/bin/env bash
# Fetch the Whisper model files pinned in voice/MODEL.lock.json into <dist> so the release
# hosts a verified mirror next to the CLI archives (voice-spec §6.1), and copy the lock file
# itself as the MODEL.lock.json asset that scripts/install.sh reads.
#
#   fetch-voice-models.sh --dist DIR [--tiers "turbo small base"] [--lock PATH] [--cache DIR]
#
# Every file is verified against the lock's size and SHA-256 before it lands in <dist>; a
# mismatch fails the run. `--cache` keeps downloads between runs (developer machines).

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

dist='' tiers='' lock="$here/../../voice/MODEL.lock.json" cache=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dist) dist=$2; shift 2 ;;
    --tiers) tiers=$2; shift 2 ;;
    --lock) lock=$2; shift 2 ;;
    --cache) cache=$2; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ -n "$dist" ]] || die "--dist is required"
[[ -f "$lock" ]] || die "lock file not found: $lock"
need_cmd jq
need_cmd curl
mkdir -p "$dist"
[[ -z "$tiers" ]] && tiers=$(jq -r '.tiers[]' "$lock" | tr '\n' ' ')

for tier in $tiers; do
  file=$(jq -r --arg t "$tier" '.models[$t].file // empty' "$lock")
  size=$(jq -r --arg t "$tier" '.models[$t].size // empty' "$lock")
  sha=$(jq -r --arg t "$tier" '.models[$t].sha256 // empty' "$lock")
  url=$(jq -r --arg t "$tier" '.models[$t].upstream_url // empty' "$lock")
  if [[ -z "$file" || -z "$size" || -z "$url" ]] || ! is_sha256 "$sha"; then
    die "lock has no complete pin for tier $tier"
  fi
  src=''
  if [[ -n "$cache" && -f "$cache/$file" ]]; then
    src="$cache/$file"
  fi
  if [[ -z "$src" ]]; then
    dest_dir=${cache:-$dist}
    mkdir -p "$dest_dir"
    log "downloading $file ($((size / 1048576)) MiB) for tier $tier"
    curl -fSL --retry 3 -C - -o "$dest_dir/$file.partial" "$url" || curl -fSL --retry 3 -o "$dest_dir/$file.partial" "$url"
    mv -f "$dest_dir/$file.partial" "$dest_dir/$file"
    src="$dest_dir/$file"
  fi
  actual_size=$(file_size "$src")
  actual_sha=$(sha256_file "$src")
  [[ "$actual_size" == "$size" ]] || die "$file: size $actual_size, lock pins $size"
  [[ "$actual_sha" == "$sha" ]] || die "$file: sha256 $actual_sha, lock pins $sha"
  if [[ "$src" != "$dist/$file" ]]; then
    cp "$src" "$dist/$file"
  fi
  log "$file verified (tier $tier)"
done
cp "$lock" "$dist/MODEL.lock.json"
log "wrote $dist/MODEL.lock.json"
