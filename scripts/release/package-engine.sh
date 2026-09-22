#!/usr/bin/env bash
# Stage the built voice helper as `voice-engine` and archive it as
#   <out>/voice-engine-<version>-<platform>.tar.gz   (flat: voice-engine[.exe], NOTICE, LICENSE)
#
# Usage:
#   package-engine.sh --version V --platform P --out DIR (--bin PATH | --target-dir DIR) [--repo-root DIR]
#
#   --target-dir  cargo output dir of the voice/engine workspace, e.g.
#                 voice/engine/target/x86_64-unknown-linux-gnu/release
#
# NOTICE is voice/NOTICE (Whisper weights and whisper.cpp are MIT). macOS binaries are ad-hoc
# signed like the CLI; install.sh clears the quarantine attribute. Prints the archive path.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

ENGINE_BIN="voice-engine"
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
  bin="$target_dir/$ENGINE_BIN$exe"
fi
[[ -f "$bin" ]] || die "voice-engine binary not found: $bin"
log "packaging $bin as $ENGINE_BIN$exe ($version, $platform)"

need_cmd tar
mkdir -p "$out"
out=$(cd "$out" && pwd) # tar runs from inside the stage dir; a relative --out would land there
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

cp "$bin" "$stage/$ENGINE_BIN$exe"
chmod 755 "$stage/$ENGINE_BIN$exe"
[[ -f "$repo_root/voice/NOTICE" ]] && cp "$repo_root/voice/NOTICE" "$stage/NOTICE"
[[ -f "$repo_root/LICENSE" ]] && cp "$repo_root/LICENSE" "$stage/LICENSE"

if [[ "$platform" == macos-* && "$(uname -s)" == Darwin ]] && command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$stage/$ENGINE_BIN"
  log "ad-hoc code signature applied"
fi

archive="$out/$ENGINE_BIN-$version-$platform.tar.gz"
(cd "$stage" && COPYFILE_DISABLE=1 tar -czf "$archive" -- *)
log "wrote $archive ($(file_size "$archive") bytes)"
printf '%s\n' "$archive"
