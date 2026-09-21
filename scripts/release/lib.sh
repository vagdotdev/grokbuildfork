# shellcheck shell=bash
# Shared helpers for scripts/release/*.sh. Source this file; do not execute it.
#
# Every script in this directory is bash (GitHub runners + maintainer machines).
# scripts/install.sh is deliberately NOT built on this file: it is POSIX sh and
# self-contained because it is piped into `sh` on user machines.

set -euo pipefail

# shellcheck disable=SC2034  # consumed by the sourcing scripts
PRODUCT_BIN="workshop"
CHANNEL_BRANCH="release-channel"
# shellcheck disable=SC2034
MANIFEST_SCHEMA_VERSION=1
# Placeholder until the public release repo exists. Overridden everywhere by
# WORKSHOP_RELEASE_REPO (repo variable in Actions, env var for the scripts).
# shellcheck disable=SC2034
DEFAULT_RELEASE_REPO="vagdotdev/grokbuildfork"

log() { printf 'release: %s\n' "$*" >&2; }
die() { printf 'release: error: %s\n' "$*" >&2; exit 1; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }

SEMVER_RE='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
is_semver() { [[ "$1" =~ $SEMVER_RE ]]; }
is_repo_slug() { [[ "$1" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; }
is_platform() { [[ "$1" =~ ^(linux|macos|windows)-(x86_64|aarch64)$ ]]; }
is_channel() { [[ "$1" =~ ^(stable|alpha)$ ]]; }
is_sha256() { [[ "$1" =~ ^[0-9a-f]{64}$ ]]; }

# Prerelease identifiers of a semver string ("" when it is a release).
semver_prerelease() {
  local v=${1%%+*}
  [[ "$v" == *-* ]] && printf '%s' "${v#*-}"
  return 0
}

# Channel a version publishes to: any prerelease -> alpha, otherwise stable.
channel_for_version() {
  if [[ -n "$(semver_prerelease "$1")" ]]; then echo alpha; else echo stable; fi
}

# semver_cmp A B -> prints -1, 0 or 1 (semver 2.0.0 precedence; build metadata ignored).
semver_cmp() {
  local a=${1%%+*} b=${2%%+*}
  is_semver "$a" || die "not a semver version: $1"
  is_semver "$b" || die "not a semver version: $2"
  local acore=${a%%-*} bcore=${b%%-*} apre='' bpre=''
  [[ "$a" == *-* ]] && apre=${a#*-}
  [[ "$b" == *-* ]] && bpre=${b#*-}

  local -a ac bc ap bp
  IFS=. read -r -a ac <<<"$acore"
  IFS=. read -r -a bc <<<"$bcore"
  local i
  for i in 0 1 2; do
    if ((ac[i] > bc[i])); then echo 1; return; fi
    if ((ac[i] < bc[i])); then echo -1; return; fi
  done

  if [[ -z "$apre" && -z "$bpre" ]]; then echo 0; return; fi
  if [[ -z "$apre" ]]; then echo 1; return; fi
  if [[ -z "$bpre" ]]; then echo -1; return; fi

  IFS=. read -r -a ap <<<"$apre"
  IFS=. read -r -a bp <<<"$bpre"
  local n=${#ap[@]} x y
  ((${#bp[@]} > n)) && n=${#bp[@]}
  for ((i = 0; i < n; i++)); do
    x=${ap[i]-}
    y=${bp[i]-}
    if [[ -z "$x" ]]; then echo -1; return; fi
    if [[ -z "$y" ]]; then echo 1; return; fi
    if [[ "$x" =~ ^[0-9]+$ && "$y" =~ ^[0-9]+$ ]]; then
      if ((10#$x > 10#$y)); then echo 1; return; fi
      if ((10#$x < 10#$y)); then echo -1; return; fi
    elif [[ "$x" =~ ^[0-9]+$ ]]; then
      echo -1; return
    elif [[ "$y" =~ ^[0-9]+$ ]]; then
      echo 1; return
    else
      if [[ "$x" > "$y" ]]; then echo 1; return; fi
      if [[ "$x" < "$y" ]]; then echo -1; return; fi
    fi
  done
  echo 0
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  fi
}

file_size() {
  if stat -c %s "$1" >/dev/null 2>&1; then stat -c %s "$1"; else stat -f %z "$1"; fi
}

# Archive and on-disk names. Shared with scripts/install.sh (keep in sync):
#   asset:  workshop-<version>-<platform>.tar.gz
#   binary: workshop-<version>-<platform>   (versioned file under $WORKSHOP_HOME/downloads)
asset_name() { printf '%s-%s-%s.tar.gz' "$PRODUCT_BIN" "$1" "$2"; }
versioned_bin_name() { printf '%s-%s-%s' "$PRODUCT_BIN" "$1" "$2"; }

release_url() { printf 'https://github.com/%s/releases/tag/%s' "$1" "$2"; }
release_download_base() { printf 'https://github.com/%s/releases/download/%s' "$1" "$2"; }
channel_raw_base() { printf 'https://raw.githubusercontent.com/%s/%s' "$1" "$CHANNEL_BRANCH"; }

utc_now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
