#!/usr/bin/env bash
# Stage a built binary as `workshop` and archive it as
#   <out>/workshop-<version>-<platform>.tar.gz   (flat: workshop[.exe], LICENSE, THIRD-PARTY-NOTICES)
#
# Usage:
#   package.sh --version V --platform P --out DIR (--bin PATH | --target-dir DIR) [--repo-root DIR]
#
#   --target-dir  cargo output dir, e.g. target/x86_64-unknown-linux-gnu/release-dist.
#                 Looks for `workshop` first (tree after the branding patch) and falls
#                 back to upstream's `xai-grok-pager`, so the pipeline works before and
#                 after the M0 overlay renames the [[bin]].
#
# macOS: the binary is ad-hoc signed (`codesign -s -`). There is deliberately no
# Developer ID signing and no notarization; install.sh clears the quarantine
# attribute instead. Prints the archive path on stdout.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

version='' platform='' out='' bin='' target_dir='' repo_root=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --platform) platform=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --bin) bin=$2; shift 2 ;;
    --target-dir) target_dir=$2; shift 2 ;;
    --repo-root) repo_root=$2; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done

is_semver "$version" || die "--version must be semver, got '$version'"
is_platform "$platform" || die "--platform must be <linux|macos|windows>-<x86_64|aarch64>, got '$platform'"
[[ -n "$out" ]] || die "--out is required"
repo_root=${repo_root:-$(cd "$here/../.." && pwd)}

exe=''
[[ "$platform" == windows-* ]] && exe='.exe'

if [[ -z "$bin" ]]; then
  [[ -n "$target_dir" ]] || die "one of --bin or --target-dir is required"
  for candidate in "$PRODUCT_BIN$exe" "xai-grok-pager$exe"; do
    if [[ -f "$target_dir/$candidate" ]]; then
      bin="$target_dir/$candidate"
      break
    fi
  done
  [[ -n "$bin" ]] || die "no $PRODUCT_BIN$exe or xai-grok-pager$exe binary in $target_dir"
fi
[[ -f "$bin" ]] || die "binary not found: $bin"
log "packaging $bin as $PRODUCT_BIN$exe ($version, $platform)"

need_cmd tar
mkdir -p "$out"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

cp "$bin" "$stage/$PRODUCT_BIN$exe"
chmod 755 "$stage/$PRODUCT_BIN$exe"
for notice in LICENSE THIRD-PARTY-NOTICES; do
  if [[ -f "$repo_root/$notice" ]]; then
    cp "$repo_root/$notice" "$stage/$notice"
  else
    log "warning: $notice not found in $repo_root; archive ships without it"
  fi
done

if [[ "$platform" == macos-* && "$(uname -s)" == Darwin ]] && command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$stage/$PRODUCT_BIN"
  log "ad-hoc code signature applied (no Developer ID; see install.sh quarantine handling)"
fi

archive="$out/$(asset_name "$version" "$platform")"
# COPYFILE_DISABLE keeps macOS from adding ._* resource-fork entries.
(cd "$stage" && COPYFILE_DISABLE=1 tar -czf "$archive" -- *)
log "wrote $archive ($(file_size "$archive") bytes)"
printf '%s\n' "$archive"
