#!/bin/sh
# Workshop installer (macOS and Linux).
#
#   curl -fsSL https://raw.githubusercontent.com/OWNER/NAME/release-channel/install.sh | sh
#
# What it does, in order:
#   1. detect OS and CPU (macOS/Linux, x86_64/aarch64; Rosetta is corrected to arm64)
#   2. fetch the channel manifest -> version, archive URL, SHA-256
#   3. download the archive, verify its SHA-256, extract `workshop`
#   4. install $WORKSHOP_HOME/downloads/workshop-<version>-<platform>, point the symlink
#      $WORKSHOP_HOME/bin/workshop at it (replacing whatever was there, a plain file from a
#      manual tar install included), stamp $WORKSHOP_HOME/installed-version
#   5. macOS: clear the quarantine attribute; run `workshop --version`
#   6. say what happened (`Installed Workshop 0.2.2` / `Updated Workshop 0.2.0 -> 0.2.2`) and
#      what to do next: `cd <project> && workshop`
#
# That is all a default install downloads: the one `workshop` archive. Voice dictation (the
# `voice-engine` helper and the Whisper speech model, up to ~570 MB) is NOT installed here;
# Workshop fetches both later, on its own. Only WORKSHOP_VOICE=1 (or a forced
# WORKSHOP_VOICE_TIER) makes the installer do it now: it picks the tier for this machine (Apple
# Silicon -> turbo; otherwise a timed probe decode on `base` decides between turbo/small/base),
# downloads that model into $WORKSHOP_HOME/voice with resume + SHA-256 verification (three
# attempts, project mirror first, then Hugging Face), and records the choice. A matching file
# is never downloaded again.
#
# The output reads like a product ("Verifying... done"), not a log.
#
# Network: manifest (or SHA256SUMS when WORKSHOP_VERSION pins a version) and the CLI archive,
# from the release repo. With WORKSHOP_VOICE=1 also SHA256SUMS, MODEL.lock.json, the helper
# archive and the model file(s) (models fall back to huggingface.co). No telemetry.
#
# Environment:
#   WORKSHOP_CHANNEL        stable (default) or alpha
#   WORKSHOP_VERSION        install this exact version instead of the channel's latest
#   WORKSHOP_HOME           install root (default: ~/.workshop)
#   WORKSHOP_RELEASE_REPO   GitHub OWNER/NAME that hosts the releases
#   WORKSHOP_MANIFEST_URL   full manifest URL (mirrors, tests)
#   WORKSHOP_DOWNLOAD_BASE  asset base containing v<version>/ directories, for pinned
#                           installs (mirrors, tests; default: the GitHub release assets)
#   WORKSHOP_VOICE=1        also install voice dictation now (helper + speech model)
#
# This file is POSIX sh on purpose: it runs under whatever `sh` the user has.
set -eu

# Stamped by scripts/release/publish-channel.sh with the repo the manifest lives in.
WORKSHOP_RELEASE_REPO_DEFAULT="vagdotdev/grokbuildfork"
CHANNEL_BRANCH="release-channel"
BIN="workshop"

say() { printf '%s\n' "$*" >&2; }
# One phase on one line: `Verifying... ` then `done` (or a short result) when it ends.
begin() { printf '%s… ' "$1" >&2; }
finish() { printf '%s\n' "${1:-done}" >&2; }
die() { printf '\nCould not install Workshop: %s\n' "$*" >&2; exit 1; }
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

# Small side downloads (manifest, checksums, pins): no progress bar, errors still reported.
fetch_quiet() {
  check_url "$1"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https,http' --proto-redir '=https' --retry 3 -o "$2" "$1"
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
  case "$PLATFORM" in
    macos-aarch64) PLATFORM_LABEL="macOS, Apple Silicon" ;;
    macos-x86_64) PLATFORM_LABEL="macOS, Intel" ;;
    *) PLATFORM_LABEL="$os $arch" ;;
  esac
}

# The Workshop already at $bindir/$BIN, if its version can be told: from the installer's own
# symlink name (downloads/workshop-<version>-<platform>), else from the version stamp this
# installer leaves. A plain binary from a manual tar install has neither, so nothing is claimed
# about its version (`workshop --version` still reports the upstream build number).
previous_version() {
  p="$bindir/$BIN"
  if [ -L "$p" ]; then
    t=$(basename "$(readlink "$p")")
    v=${t#"$BIN-"}
    v=${v%"-$PLATFORM"}
    if is_semver "$v"; then printf '%s' "$v"; return 0; fi
  fi
  if [ -f "$home/installed-version" ]; then
    v=$(tr -d '\n\r ' <"$home/installed-version")
    if is_semver "$v"; then printf '%s' "$v"; return 0; fi
  fi
  return 1
}

# ---------------------------------------------------------------------------
# Voice dictation: helper + model, only on request (voice/MODEL.lock.json in the source tree is
# the pin; the release ships it as the MODEL.lock.json asset so both installer and app read one
# file). A default install touches none of this: Workshop fetches voice on its own later.
#
#   WORKSHOP_VOICE=1                 install the helper and download the model now
#   WORKSHOP_VOICE_TIER=turbo|small|base   force the tier, skip the hardware probe (implies WORKSHOP_VOICE=1)
# Undocumented CI escape hatches (never needed by users; never printed):
#   WORKSHOP_VOICE_SKIP=1            never install voice, even with WORKSHOP_VOICE=1
#   WORKSHOP_VOICE_MODEL_BASE=URL    base URL for the model mirror (default: the release assets)
#   WORKSHOP_VOICE_UPSTREAM_BASE=URL base URL replacing https://huggingface.co/... upstream files
# ---------------------------------------------------------------------------
VOICE_ENGINE_BIN="voice-engine"

# Whether this run installs voice dictation: only when asked for.
voice_requested() {
  [ "${WORKSHOP_VOICE_SKIP:-0}" = 1 ] && return 1
  [ "${WORKSHOP_VOICE:-0}" = 1 ] || [ -n "${WORKSHOP_VOICE_TIER:-}" ]
}

# Number under `"KEY": 123` inside a JSON block.
block_num() {
  printf '%s' "$1" | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\([0-9][0-9.]*\).*/\1/p"
}

file_size_of() {
  if stat -c %s "$1" >/dev/null 2>&1; then stat -c %s "$1"; else stat -f %z "$1"; fi
}

# Resumable download: continues an existing $2 when the server supports ranges, restarts otherwise.
fetch_resume() {
  check_url "$1"
  if command -v curl >/dev/null 2>&1; then
    if [ -t 2 ]; then progress="--progress-bar"; else progress="-s"; fi
    rc=0
    curl -fSL "$progress" --proto '=https,http' --proto-redir '=https' --retry 2 -C - -o "$2" "$1" || rc=$?
    [ "$rc" -eq 0 ] && return 0
    # 33: the server ignored the range (or the partial is already complete). Start over once.
    if [ "$rc" -eq 33 ]; then
      rm -f "$2"
      rc=0
      curl -fSL "$progress" --proto '=https,http' --proto-redir '=https' --retry 2 -o "$2" "$1" || rc=$?
    fi
    return "$rc"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -c -O "$2" "$1"
  else
    die "need curl or wget"
  fi
}

# Size of a regular file in bytes; 0 when it does not exist.
partial_size() {
  if [ -f "$1" ]; then file_size_of "$1"; else echo 0; fi
}

# Available KiB on the volume holding $1 (or its nearest existing parent).
avail_kib() {
  d=$1
  while [ ! -d "$d" ]; do d=$(dirname "$d"); done
  df -Pk "$d" | awk 'NR==2 {print $4}'
}

# Total RAM in bytes, or empty when unknown.
total_ram_bytes() {
  if [ -r /proc/meminfo ]; then
    awk '/^MemTotal:/ {print $2 * 1024; exit}' /proc/meminfo
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.memsize 2>/dev/null || true
  fi
}

# voice_model_pins TIER -> sets VM_FILE VM_SIZE VM_SHA VM_UPSTREAM from the lock file ($VOICE_LOCK).
voice_model_pins() {
  block=$(json_block "$VOICE_LOCK" "$1")
  [ -n "$block" ] || die "MODEL.lock.json has no model tier '$1'"
  VM_FILE=$(block_str "$block" file)
  VM_SIZE=$(block_num "$block" size)
  VM_SHA=$(block_str "$block" sha256)
  VM_UPSTREAM=$(block_str "$block" upstream_url)
  if [ -z "$VM_FILE" ] || [ -z "$VM_SIZE" ] || ! is_sha256 "$VM_SHA" || [ -z "$VM_UPSTREAM" ]; then
    die "MODEL.lock.json tier '$1' is incomplete (file/size/sha256/upstream_url)"
  fi
  if [ -n "${WORKSHOP_VOICE_UPSTREAM_BASE:-}" ]; then
    VM_UPSTREAM="${WORKSHOP_VOICE_UPSTREAM_BASE%/}/$VM_FILE"
  fi
}

# voice_model_verified TIER DEST_DIR -> 0 when DEST_DIR/<file> exists with the pinned size and SHA-256.
voice_model_verified() {
  voice_model_pins "$1"
  f="$2/$VM_FILE"
  [ -f "$f" ] && [ "$(file_size_of "$f")" = "$VM_SIZE" ] && [ "$(sha256_of "$f")" = "$VM_SHA" ]
}

# voice_fetch_model TIER DEST_DIR MIRROR_BASE
# Makes DEST_DIR/<file> present with the pinned SHA-256. Prints one line; exits non-zero on failure.
voice_fetch_model() {
  voice_model_pins "$1"
  dest="$2/$VM_FILE"
  partial="$dest.partial"
  mkdir -p "$2"
  if [ -f "$dest" ]; then
    if [ "$(file_size_of "$dest")" = "$VM_SIZE" ] && [ "$(sha256_of "$dest")" = "$VM_SHA" ]; then
      say "Voice model already present."
      return 0
    fi
    say "voice model $VM_FILE failed verification; replacing it"
    rm -f "$dest"
  fi
  have=0
  [ -f "$partial" ] && have=$(file_size_of "$partial")
  if [ "$have" -ge "$VM_SIZE" ]; then rm -f "$partial"; have=0; fi
  need_kib=$(( (VM_SIZE - have) / 1024 + 65536 ))
  if [ "$(avail_kib "$2")" -lt "$need_kib" ]; then
    die "not enough disk space in $2 for the voice model ($(( VM_SIZE / 1048576 )) MiB needed); free some space and re-run this command"
  fi
  say "Downloading the voice model ($VM_FILE, $(( VM_SIZE / 1048576 )) MiB)..."
  mirror="${3%/}/$VM_FILE"
  attempt=1
  while [ "$attempt" -le 3 ]; do
    # Mirror first, then upstream, then the mirror again.
    if [ $((attempt % 2)) -eq 1 ]; then url=$mirror; else url=$VM_UPSTREAM; fi
    if fetch_resume "$url" "$partial" 2>"$partial.err"; then
      got=$(partial_size "$partial")
      if [ "$got" = "$VM_SIZE" ]; then
        actual=$(sha256_of "$partial")
        if [ "$actual" = "$VM_SHA" ]; then
          rm -f "$partial.err"
          mv -f "$partial" "$dest"
          say "Voice model ready."
          return 0
        fi
        say "voice model checksum mismatch from $url (attempt $attempt): expected $VM_SHA, got $actual"
        rm -f "$partial"
      else
        say "voice model download incomplete from $url (attempt $attempt): $got of $VM_SIZE bytes; will resume"
      fi
    else
      say "voice model download failed from $url (attempt $attempt): $(tr -d '\r' <"$partial.err" | tail -n 1)"
    fi
    attempt=$((attempt + 1))
  done
  rm -f "$partial.err"
  die "could not download the voice model after 3 attempts. Check your network and re-run the same install command; it resumes where it stopped."
}

# voice_probe_ms MODEL_PATH -> prints the helper's one-second probe decode time in ms.
voice_probe_ms() {
  out=$("$VOICE_ENGINE_PATH" --probe --probe-language en --model "$1" 2>/dev/null) || return 1
  printf '%s' "$out" | tr -d '\n\r' | sed -n 's/.*"probe_ms"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p'
}

# Silent per-machine model tier (voice-spec §9.6). Sets VOICE_TIER; may download `base` as the probe yardstick.
voice_pick_tier() {
  sel=$(json_block "$VOICE_LOCK" selection)
  budget=$(block_num "$sel" interim_budget_ms)
  min_ram=$(block_num "$sel" min_ram_bytes_for_probe)
  ratio_small=$(block_num "$sel" probe_ratio_small_over_base)
  ratio_turbo=$(block_num "$sel" probe_ratio_turbo_over_base)
  [ -n "$budget" ] || budget=1000
  [ -n "$ratio_small" ] || ratio_small=3.9
  [ -n "$ratio_turbo" ] || ratio_turbo=19.5

  VOICE_TIER_READY=0
  if [ -n "${WORKSHOP_VOICE_TIER:-}" ]; then
    case "$WORKSHOP_VOICE_TIER" in turbo | small | base) VOICE_TIER=$WORKSHOP_VOICE_TIER; return 0 ;; esac
    die "WORKSHOP_VOICE_TIER must be turbo, small or base"
  fi
  if [ "$PLATFORM" = macos-aarch64 ]; then
    VOICE_TIER=turbo # Metal
    return 0
  fi
  ram=$(total_ram_bytes)
  if [ -n "$ram" ] && [ -n "$min_ram" ] && [ "$ram" -lt "${min_ram%.*}" ]; then
    VOICE_TIER=base
    return 0
  fi
  # CPU machine: install the smallest tier (every machine can run it; it is also the step-down floor),
  # time one interim decode on it, and predict the bigger tiers from the lock file's ratios.
  voice_fetch_model base "$VOICE_DIR" "$VOICE_MODEL_BASE"
  base_file="$VOICE_DIR/$VM_FILE"
  base_ms=$(voice_probe_ms "$base_file") || base_ms=""
  VOICE_TIER=base
  VOICE_TIER_READY=1
  if [ -z "$base_ms" ]; then
    return 0
  fi
  predicted=$(awk -v b="$base_ms" -v s="$ratio_small" -v t="$ratio_turbo" -v budget="$budget" \
    'BEGIN { if (b * t <= budget) print "turbo"; else if (b * s <= budget) print "small"; else print "base" }')
  [ "$predicted" = base ] && return 0
  voice_fetch_model "$predicted" "$VOICE_DIR" "$VOICE_MODEL_BASE"
  tier_ms=$(voice_probe_ms "$VOICE_DIR/$VM_FILE") || tier_ms=""
  if [ -n "$tier_ms" ] && [ "$tier_ms" -le "$budget" ]; then
    VOICE_TIER=$predicted
  else
    # Prediction was optimistic: keep base, drop the file that cannot keep up here.
    rm -f "$VOICE_DIR/$VM_FILE"
  fi
}

# install_voice VERSION PLATFORM ASSET_BASE SHA256SUMS_PATH (only when voice_requested)
install_voice() {
  v_version=$1
  v_platform=$2
  v_base=${3%/}
  v_sums=$4
  VOICE_DIR="$home/voice"
  VOICE_MODEL_BASE="${WORKSHOP_VOICE_MODEL_BASE:-$v_base}"

  # 1. helper beside the CLI (same downloads/ + bin/ symlink layout as workshop itself)
  begin "Installing the voice helper"
  asset="$VOICE_ENGINE_BIN-$v_version-$v_platform.tar.gz"
  sha=$(awk -v n="$asset" '$2 == n {print $1}' "$v_sums")
  is_sha256 "$sha" || die "release $v_version has no voice helper for $v_platform (SHA256SUMS lists no $asset); /voice cannot work without it"
  fetch_quiet "$v_base/$asset" "$tmp/$asset"
  actual=$(sha256_of "$tmp/$asset")
  [ "$actual" = "$sha" ] || die "checksum mismatch for $asset
  expected: $sha
  actual:   $actual"
  mkdir -p "$tmp/ve"
  tar -xzf "$tmp/$asset" -C "$tmp/ve"
  [ -f "$tmp/ve/$VOICE_ENGINE_BIN" ] || die "archive does not contain a $VOICE_ENGINE_BIN binary"
  vname="$VOICE_ENGINE_BIN-$v_version-$v_platform"
  chmod 755 "$tmp/ve/$VOICE_ENGINE_BIN"
  mv -f "$tmp/ve/$VOICE_ENGINE_BIN" "$downloads/$vname.tmp.$$"
  mv -f "$downloads/$vname.tmp.$$" "$downloads/$vname"
  if [ "$OS" = macos ] && command -v xattr >/dev/null 2>&1; then
    xattr -d com.apple.quarantine "$downloads/$vname" 2>/dev/null || true
  fi
  ln -s "../downloads/$vname" "$bindir/.$VOICE_ENGINE_BIN.tmp.$$"
  mv -f "$bindir/.$VOICE_ENGINE_BIN.tmp.$$" "$bindir/$VOICE_ENGINE_BIN"
  VOICE_ENGINE_PATH="$bindir/$VOICE_ENGINE_BIN"
  if ! ve_version=$("$VOICE_ENGINE_PATH" --version 2>&1); then
    die "$VOICE_ENGINE_PATH --version failed:
$ve_version"
  fi
  finish "done ($ve_version)"

  # 2. model pins
  VOICE_LOCK="$tmp/MODEL.lock.json"
  fetch_quiet "$v_base/MODEL.lock.json" "$VOICE_LOCK"
  [ -n "$(json_block "$VOICE_LOCK" base)" ] || die "release $v_version ships no MODEL.lock.json with model pins"

  # 3. an earlier install (or the app) already chose a tier and its file verifies: nothing to download
  if [ -z "${WORKSHOP_VOICE_TIER:-}" ] && [ -f "$VOICE_DIR/model.selected" ]; then
    selected=$(tr -d '\n\r ' <"$VOICE_DIR/model.selected")
    case "$selected" in
      turbo | small | base)
        if voice_model_verified "$selected" "$VOICE_DIR"; then
          say "Voice model already present."
          return 0
        fi
        ;;
    esac
  fi

  # 4. pick the tier for this machine, make its file present and verified, remember the choice
  voice_pick_tier
  [ "$VOICE_TIER_READY" = 1 ] || voice_fetch_model "$VOICE_TIER" "$VOICE_DIR" "$VOICE_MODEL_BASE"
  printf '%s\n' "$VOICE_TIER" >"$VOICE_DIR/.model.selected.tmp.$$"
  mv -f "$VOICE_DIR/.model.selected.tmp.$$" "$VOICE_DIR/model.selected"
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
  downloads="$home/downloads"
  bindir="$home/bin"

  detect_platform
  # What is there now, before anything is replaced: a version when it can be told, and whether
  # anything (symlink, plain file, even a dangling link) sits at bin/workshop at all.
  previous=$(previous_version || true)
  had_previous=0
  if [ -e "$bindir/$BIN" ] || [ -L "$bindir/$BIN" ]; then had_previous=1; fi

  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t workshop)
  trap 'rm -rf "$tmp"' EXIT INT TERM

  begin "Finding the latest Workshop"
  if [ -n "$pinned" ]; then
    is_semver "$pinned" || die "WORKSHOP_VERSION must be a semver version like 1.2.3 or 1.2.3-alpha.1"
    version=$pinned
    asset="$BIN-$version-$PLATFORM.tar.gz"
    base="${WORKSHOP_DOWNLOAD_BASE:-https://github.com/$repo/releases/download}"
    url="$base/v$version/$asset"
    fetch_quiet "$base/v$version/SHA256SUMS" "$tmp/SHA256SUMS"
    sha=$(awk -v n="$asset" '$2 == n {print $1}' "$tmp/SHA256SUMS")
    [ -n "$sha" ] || die "v$version has no asset for $PLATFORM (SHA256SUMS lists no $asset)"
  else
    manifest_url="${WORKSHOP_MANIFEST_URL:-https://raw.githubusercontent.com/$repo/$CHANNEL_BRANCH/$channel.json}"
    fetch_quiet "$manifest_url" "$tmp/manifest.json"
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
  finish "$version ($PLATFORM_LABEL)"
  # Every release asset (CLI, voice helper, model mirror) lives next to the CLI archive; the
  # checksum list is only needed for the voice helper.
  asset_base=${url%/*}
  if voice_requested && [ ! -f "$tmp/SHA256SUMS" ]; then
    fetch_quiet "$asset_base/SHA256SUMS" "$tmp/SHA256SUMS"
  fi

  # The archive is the one download a default install makes; its progress bar (on a terminal)
  # takes the line under this one.
  say "Downloading Workshop $version…"
  fetch "$url" "$tmp/$asset"
  begin "Verifying"
  actual=$(sha256_of "$tmp/$asset")
  if [ "$actual" != "$sha" ]; then
    die "checksum mismatch for $asset
  expected: $sha
  actual:   $actual
The download is corrupt or tampered with; nothing was installed."
  fi
  finish

  begin "Installing"
  mkdir -p "$tmp/x"
  tar -xzf "$tmp/$asset" -C "$tmp/x"
  [ -f "$tmp/x/$BIN" ] || die "archive does not contain a $BIN binary"
  mkdir -p "$downloads" "$bindir"
  name="$BIN-$version-$PLATFORM"
  chmod 755 "$tmp/x/$BIN"
  mv -f "$tmp/x/$BIN" "$downloads/$name.tmp.$$"
  mv -f "$downloads/$name.tmp.$$" "$downloads/$name"
  if [ "$OS" = macos ] && command -v xattr >/dev/null 2>&1; then
    xattr -d com.apple.quarantine "$downloads/$name" 2>/dev/null || true
  fi
  # `mv` over whatever sits at bin/workshop — the previous symlink, or a plain binary from a
  # manual tar install — replaces it in one step; a directory there is the one thing we refuse.
  [ ! -d "$bindir/$BIN" ] || [ -L "$bindir/$BIN" ] || die "$bindir/$BIN is a directory; move it aside and re-run"
  ln -s "../downloads/$name" "$bindir/.$BIN.tmp.$$"
  mv -f "$bindir/.$BIN.tmp.$$" "$bindir/$BIN"
  printf '%s\n' "$version" >"$home/.installed-version.tmp.$$"
  mv -f "$home/.installed-version.tmp.$$" "$home/installed-version"

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
  finish

  if voice_requested; then
    install_voice "$version" "$PLATFORM" "$asset_base" "$tmp/SHA256SUMS"
  fi

  say ""
  if [ -z "$previous" ]; then
    if [ "$had_previous" = 1 ]; then
      say "Replaced the existing Workshop with $version."
    else
      say "Installed Workshop $version."
    fi
  elif [ "$previous" = "$version" ]; then
    say "Workshop $version was already installed; refreshed."
  else
    say "Updated Workshop $previous → $version."
  fi
  if [ "$OS" = macos ]; then
    say "(Not Apple-notarized: if macOS ever blocks it, run  xattr -d com.apple.quarantine $bindir/$BIN)"
  fi
  case ":$PATH:" in
    *":$bindir:"*) ;;
    *)
      say ""
      say "Add Workshop to your PATH (append to ~/.zshrc, ~/.bashrc or ~/.config/fish/config.fish):"
      case "$(basename "${SHELL:-sh}")" in
        fish) printf '  fish_add_path %s\n' "$bindir" >&2 ;;
        *) printf '  export PATH="%s:%s"\n' "$bindir" "\$PATH" >&2 ;;
      esac
      ;;
  esac
  printf '\n  cd <your-project> && %s\n\n' "$BIN" >&2
  say "Then type what you want. Workshop starts on a free model; /model switches, /auth connects a subscription or an API key."
}

main "$@"
