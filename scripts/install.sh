#!/bin/sh
# Workshop installer (macOS and Linux).
#
#   curl -fsSL https://raw.githubusercontent.com/OWNER/NAME/release-channel/install.sh | sh
#
# Steps, in order:
#   1. detect OS and CPU (macOS/Linux, x86_64/aarch64; Rosetta is corrected to arm64)
#   2. fetch the channel manifest -> version, archive URL, SHA-256
#   3. download the archive, verify its SHA-256, extract `workshop`
#   4. install $WORKSHOP_HOME/downloads/workshop-<version>-<platform>
#      and point the symlink $WORKSHOP_HOME/bin/workshop at it
#   5. macOS: clear the quarantine attribute; run `workshop --version`; print a PATH hint
#
# Network: exactly two requests (manifest + archive; or archive + SHA256SUMS when
# WORKSHOP_VERSION pins a version), both to the release repo. No telemetry.
#
# Environment:
#   WORKSHOP_CHANNEL        stable (default) or alpha
#   WORKSHOP_VERSION        install this exact version instead of the channel's latest
#   WORKSHOP_HOME           install root (default: ~/.workshop)
#   WORKSHOP_RELEASE_REPO   GitHub OWNER/NAME that hosts the releases
#   WORKSHOP_MANIFEST_URL   full manifest URL (mirrors, tests)
#   WORKSHOP_DOWNLOAD_BASE  asset base containing v<version>/ directories, for pinned
#                           installs (mirrors, tests; default: the GitHub release assets)
#
# This file is POSIX sh on purpose: it runs under whatever `sh` the user has.
set -eu

# Stamped by scripts/release/publish-channel.sh with the repo the manifest lives in.
WORKSHOP_RELEASE_REPO_DEFAULT="vagdotdev/grokbuildfork"
CHANNEL_BRANCH="release-channel"
BIN="workshop"

say() { printf 'workshop: %s\n' "$*" >&2; }
die() { printf 'workshop: error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }

check_url() {
  case "$1" in
    https://*) ;;
    http://127.0.0.1[:/]* | http://localhost[:/]* | "http://[::1]"[:/]*) ;;
    *) die "refusing to download from a non-https URL: $1" ;;
  esac
}

fetch() {
  check_url "$1"
  if command -v curl >/dev/null 2>&1; then
    if [ -t 2 ]; then progress="--progress-bar"; else progress="-s"; fi
    curl -fSL "$progress" --proto '=https,http' --proto-redir '=https' --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    die "need curl or wget"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    die "need sha256sum, shasum or openssl to verify the download"
  fi
}

# First `"KEY": "value"` string in a JSON file (manifests are small and flat).
json_str() {
  tr -d '\n\r' <"$1" | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p"
}
# Body of the object under `"KEY": { ... }` (artifact entries contain no nested objects).
json_block() {
  tr -d '\n\r' <"$1" | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*{\([^}]*\)}.*/\1/p"
}
block_str() {
  printf '%s' "$1" | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p"
}
is_semver() { printf '%s' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; }
is_sha256() { printf '%s' "$1" | grep -Eq '^[0-9a-f]{64}$'; }

detect_platform() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin) os=macos ;;
    Linux) os=linux ;;
    MINGW* | MSYS* | CYGWIN*) die "Windows is best-effort only: download $BIN-<version>-windows-x86_64.tar.gz from the releases page by hand" ;;
    *) die "unsupported operating system: $os (Workshop ships macOS and Linux builds)" ;;
  esac
  case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) die "unsupported CPU architecture: $arch (Workshop ships x86_64 and aarch64 builds)" ;;
  esac
  if [ "$os" = macos ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
    arch=aarch64
    say "Apple Silicon detected under Rosetta; installing the native arm64 build"
  fi
  OS=$os
  PLATFORM="$os-$arch"
}

main() {
  need uname
  need tar
  need mktemp
  [ -n "${HOME:-}" ] || die "HOME is not set"

  repo="${WORKSHOP_RELEASE_REPO:-$WORKSHOP_RELEASE_REPO_DEFAULT}"
  channel="${WORKSHOP_CHANNEL:-stable}"
  case "$channel" in stable | alpha) ;; *) die "WORKSHOP_CHANNEL must be stable or alpha, got '$channel'" ;; esac
  home="${WORKSHOP_HOME:-$HOME/.workshop}"
  pinned="${WORKSHOP_VERSION:-}"

  detect_platform

  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t workshop)
  trap 'rm -rf "$tmp"' EXIT INT TERM

  if [ -n "$pinned" ]; then
    is_semver "$pinned" || die "WORKSHOP_VERSION must be a semver version like 1.2.3 or 1.2.3-alpha.1"
    version=$pinned
    asset="$BIN-$version-$PLATFORM.tar.gz"
    base="${WORKSHOP_DOWNLOAD_BASE:-https://github.com/$repo/releases/download}"
    url="$base/v$version/$asset"
    say "fetching SHA256SUMS for v$version"
    fetch "$base/v$version/SHA256SUMS" "$tmp/SHA256SUMS"
    sha=$(awk -v n="$asset" '$2 == n {print $1}' "$tmp/SHA256SUMS")
    [ -n "$sha" ] || die "v$version has no asset for $PLATFORM (SHA256SUMS lists no $asset)"
  else
    manifest_url="${WORKSHOP_MANIFEST_URL:-https://raw.githubusercontent.com/$repo/$CHANNEL_BRANCH/$channel.json}"
    say "fetching $channel channel manifest"
    fetch "$manifest_url" "$tmp/manifest.json"
    version=$(json_str "$tmp/manifest.json" version)
    is_semver "$version" || die "manifest at $manifest_url has no valid version field"
    block=$(json_block "$tmp/manifest.json" "$PLATFORM")
    [ -n "$block" ] || die "the $channel channel ($version) has no build for $PLATFORM"
    url=$(block_str "$block" url)
    sha=$(block_str "$block" sha256)
    asset=${url##*/}
  fi
  is_sha256 "$sha" || die "manifest has no valid sha256 for $PLATFORM"
  case "$asset" in *.tar.gz) ;; *) die "unexpected asset name: $asset" ;; esac

  say "downloading $BIN $version for $PLATFORM"
  fetch "$url" "$tmp/$asset"
  actual=$(sha256_of "$tmp/$asset")
  if [ "$actual" != "$sha" ]; then
    die "checksum mismatch for $asset
  expected: $sha
  actual:   $actual
The download is corrupt or tampered with; nothing was installed."
  fi
  say "checksum verified"

  mkdir -p "$tmp/x"
  tar -xzf "$tmp/$asset" -C "$tmp/x"
  [ -f "$tmp/x/$BIN" ] || die "archive does not contain a $BIN binary"

  downloads="$home/downloads"
  bindir="$home/bin"
  mkdir -p "$downloads" "$bindir"
  name="$BIN-$version-$PLATFORM"
  chmod 755 "$tmp/x/$BIN"
  mv -f "$tmp/x/$BIN" "$downloads/$name.tmp.$$"
  mv -f "$downloads/$name.tmp.$$" "$downloads/$name"
  if [ "$OS" = macos ] && command -v xattr >/dev/null 2>&1; then
    xattr -d com.apple.quarantine "$downloads/$name" 2>/dev/null || true
  fi
  ln -s "../downloads/$name" "$bindir/.$BIN.tmp.$$"
  mv -f "$bindir/.$BIN.tmp.$$" "$bindir/$BIN"
  say "installed $bindir/$BIN -> downloads/$name"

  if ! reported=$("$bindir/$BIN" --version 2>&1); then
    if [ "$OS" = macos ]; then
      die "$bindir/$BIN did not start. Workshop is not Apple-notarized; if macOS blocked it, run:
  xattr -d com.apple.quarantine $bindir/$BIN
then retry \`$BIN --version\`. Output was:
$reported"
    fi
    die "$bindir/$BIN --version failed:
$reported"
  fi
  say "$reported"
  if [ "$OS" = macos ]; then
    say "macOS note: this build is not Apple-notarized. If it is ever blocked, run: xattr -d com.apple.quarantine $bindir/$BIN"
  fi

  case ":$PATH:" in
    *":$bindir:"*) say "run: $BIN" ;;
    *)
      say "add Workshop to your PATH (append to ~/.zshrc, ~/.bashrc or ~/.config/fish/config.fish), then run: $BIN"
      case "$(basename "${SHELL:-sh}")" in
        fish) printf '  fish_add_path %s\n' "$bindir" >&2 ;;
        *) printf '  export PATH="%s:%s"\n' "$bindir" "\$PATH" >&2 ;;
      esac
      ;;
  esac
}

main "$@"
