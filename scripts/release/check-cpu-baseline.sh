#!/usr/bin/env bash
# The voice-engine helpers must run on the oldest CPU of each platform, whatever CPU the release
# runner happened to have. whisper.cpp compiles for the build machine (`-march=native` /
# `-mcpu=native`) unless GGML_NATIVE=OFF: v0.2.1 shipped a linux-x86_64 helper full of AVX-512 and
# a linux-aarch64 helper with SVE and I8MM, which die with SIGILL on most laptops, Raspberry Pis,
# Graviton2 and Linux VMs on Apple Silicon. This scans each helper's disassembly for instruction
# families beyond its platform's baseline and fails the release when it finds any.
#
#   check-cpu-baseline.sh --dist DIR --version V [--platform P ...]
#
# Baselines: x86_64 = x86-64-v3 (AVX2/FMA/F16C, no AVX-512 registers); macos-aarch64 = Apple M1
# (no SVE, I8MM, BF16); linux-aarch64 = Armv8.0-A + NEON (additionally no dot product).
# Needs llvm-objdump (reads ELF and Mach-O for both architectures).

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

dist='' version=''
platforms=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dist) dist=$2; shift 2 ;;
    --version) version=$2; shift 2 ;;
    --platform) platforms+=("$2"); shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ -d "$dist" ]] || die "--dist must be a directory"
is_semver "$version" || die "--version must be semver"
[[ ${#platforms[@]} -gt 0 ]] || platforms=(linux-x86_64 linux-aarch64 macos-x86_64 macos-aarch64)
objdump=$(command -v llvm-objdump || true)
[[ -n "$objdump" ]] || die "llvm-objdump not found (apt-get install llvm)"

SVE='\bz[0-9]+\.[bhsdq]\b|\bp[0-9]+/[zm]\b'
I8MM='\b(smmla|ummla|usmmla|usdot|sudot)\b'
BF16='\b(bfdot|bfmmla|bfcvtn2?|bfcvt|bfmlal[bt])\b'
DOTPROD='\b(sdot|udot)\b'
AVX512='%zmm[0-9]|\{%k[1-7]\}'

failed=0
for platform in "${platforms[@]}"; do
  is_platform "$platform" || die "bad platform: $platform"
  archive="$dist/voice-engine-$version-$platform.tar.gz"
  [[ -f "$archive" ]] || die "missing $archive"
  tmp=$(mktemp -d)
  tar -xzf "$archive" -C "$tmp" voice-engine
  case "$platform" in
    *-x86_64) mattr=() checks=("AVX-512:$AVX512") ;;
    macos-aarch64) mattr=(--mattr=+all) checks=("SVE:$SVE" "I8MM:$I8MM" "BF16:$BF16") ;;
    linux-aarch64) mattr=(--mattr=+all) checks=("SVE:$SVE" "I8MM:$I8MM" "BF16:$BF16" "dot product:$DOTPROD") ;;
  esac
  "$objdump" -d --no-show-raw-insn "${mattr[@]}" "$tmp/voice-engine" >"$tmp/dis.txt" 2>/dev/null ||
    die "llvm-objdump could not disassemble $archive"
  total=$(wc -l <"$tmp/dis.txt")
  [[ "$total" -gt 10000 ]] || die "disassembly of $archive is implausibly short ($total lines)"
  bad=''
  for check in "${checks[@]}"; do
    n=$(grep -cE "${check#*:}" "$tmp/dis.txt" || true)
    [[ "$n" -eq 0 ]] || bad+=" ${check%%:*}=$n"
  done
  if [[ -n "$bad" ]]; then
    printf 'FAIL  voice-engine %s uses instructions beyond the platform baseline:%s\n' "$platform" "$bad"
    failed=1
  else
    printf 'PASS  voice-engine %s stays within the baseline (%s instructions scanned)\n' "$platform" "$total"
  fi
  rm -rf "$tmp"
done
exit "$failed"
