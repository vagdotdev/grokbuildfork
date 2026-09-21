#!/usr/bin/env bash
# Write or verify <dist>/SHA256SUMS for the release assets in <dist>.
#
#   checksums.sh <dist>          writes SHA256SUMS over workshop-* files (sorted)
#   checksums.sh --check <dist>  verifies every listed file
#
# Format is the classic `<sha256>  <filename>` so users can run
# `sha256sum -c SHA256SUMS` (Linux) or `shasum -a 256 -c SHA256SUMS` (macOS).

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

check=false
if [[ "${1:-}" == --check ]]; then
  check=true
  shift
fi
dist=${1:-}
[[ -d "$dist" ]] || die "usage: checksums.sh [--check] <dist-dir>"

sums="$dist/SHA256SUMS"

if $check; then
  [[ -f "$sums" ]] || die "$sums not found"
  failed=0
  while read -r expected name; do
    [[ -n "$name" ]] || continue
    actual=$(sha256_file "$dist/$name")
    if [[ "$actual" == "$expected" ]]; then
      printf '%s: OK\n' "$name"
    else
      printf '%s: FAILED\n' "$name" >&2
      failed=1
    fi
  done <"$sums"
  ((failed == 0)) || die "checksum verification failed"
  exit 0
fi

shopt -s nullglob
files=("$dist"/"$PRODUCT_BIN"-*)
((${#files[@]} > 0)) || die "no $PRODUCT_BIN-* assets in $dist"

: >"$sums.tmp"
for f in "${files[@]}"; do
  [[ -f "$f" ]] || continue
  printf '%s  %s\n' "$(sha256_file "$f")" "$(basename "$f")" >>"$sums.tmp"
done
LC_ALL=C sort -k2 "$sums.tmp" >"$sums"
rm -f "$sums.tmp"
log "wrote $sums ($(wc -l <"$sums" | tr -d ' ') entries)"
cat "$sums"
