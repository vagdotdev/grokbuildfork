#!/usr/bin/env bash
# End-to-end installer smoke test against a local HTTP server: proves
# manifest -> download -> SHA-256 verify -> install -> `workshop --version`
# without touching GitHub. Used by the release workflow's smoke job and for
# local dry runs (any dist dir with a tarball for this host + SHA256SUMS).
#
#   smoke-install.sh --dist DIR --version V --channel <stable|alpha> [--repo OWNER/NAME] \
#                    [--install-sh PATH] [--platform P]
#
# Cases: channel manifest install, pinned-version install, tampered checksum is
# rejected, non-https manifest URL is refused. Requires python3 (http.server).

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib.sh
. "$here/lib.sh"

dist='' version='' channel='' repo="$DEFAULT_RELEASE_REPO" install_sh="$here/../install.sh" platform=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dist) dist=$2; shift 2 ;;
    --version) version=$2; shift 2 ;;
    --channel) channel=$2; shift 2 ;;
    --repo) repo=$2; shift 2 ;;
    --install-sh) install_sh=$2; shift 2 ;;
    --platform) platform=$2; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ -f "$dist/SHA256SUMS" ]] || die "--dist must contain SHA256SUMS (run checksums.sh)"
is_semver "$version" || die "--version must be semver"
is_channel "$channel" || die "--channel must be stable or alpha"
[[ -f "$install_sh" ]] || die "install.sh not found: $install_sh"
need_cmd python3
need_cmd curl
need_cmd jq

if [[ -z "$platform" ]]; then
  case "$(uname -s)" in Darwin) os=macos ;; Linux) os=linux ;; *) die "smoke test supports macOS and Linux hosts" ;; esac
  case "$(uname -m)" in x86_64 | amd64) arch=x86_64 ;; arm64 | aarch64) arch=aarch64 ;; *) die "unsupported host arch" ;; esac
  if [[ "$os" == macos && "$arch" == x86_64 && "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" == 1 ]]; then arch=aarch64; fi
  platform="$os-$arch"
fi
asset=$(asset_name "$version" "$platform")
[[ -f "$dist/$asset" ]] || die "$dist has no $asset for this host; nothing to smoke-test"

tmp=$(mktemp -d)
server_pid=''
cleanup() {
  [[ -n "$server_pid" ]] && kill "$server_pid" 2>/dev/null
  rm -rf "$tmp"
}
trap cleanup EXIT

port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
base="http://127.0.0.1:$port"
www="$tmp/www"
mkdir -p "$www/dl/v$version"
cp "$dist"/"$PRODUCT_BIN"-*.tar.gz "$dist/SHA256SUMS" "$www/dl/v$version/"

"$here/manifest.sh" --version "$version" --tag "v$version" --channel "$channel" --repo "$repo" \
  --dist "$dist" --out "$www/$channel.json" --asset-base "$base/dl/v$version" --attested false
jq --arg p "$platform" '.artifacts[$p].sha256 = ("0" * 64)' "$www/$channel.json" >"$www/tampered.json"

(cd "$www" && exec python3 -m http.server --bind 127.0.0.1 "$port" >/dev/null 2>&1) &
server_pid=$!
for _ in $(seq 1 50); do
  curl -fs -o /dev/null "$base/$channel.json" && break
  sleep 0.1
done
curl -fs -o /dev/null "$base/$channel.json" || die "local http server did not start"

pass=0
fail=0
report() {
  if [[ "$1" == ok ]]; then pass=$((pass + 1)); printf 'PASS  %s\n' "$2"; else fail=$((fail + 1)); printf 'FAIL  %s\n' "$2"; fi
}

check_install() { # check_install HOME
  local home=$1 out link
  [[ -L "$home/bin/$PRODUCT_BIN" ]] || { echo "  no symlink at $home/bin/$PRODUCT_BIN"; return 1; }
  link=$(readlink "$home/bin/$PRODUCT_BIN")
  [[ "$link" == "../downloads/$(versioned_bin_name "$version" "$platform")" ]] || { echo "  unexpected symlink target: $link"; return 1; }
  out=$("$home/bin/$PRODUCT_BIN" --version 2>&1) || { echo "  --version failed: $out"; return 1; }
  [[ "$out" == *"$version"* ]] || { echo "  --version output lacks $version: $out"; return 1; }
  echo "  $out"
}

echo "== 1. install from $channel manifest"
if (WORKSHOP_HOME="$tmp/h1" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") && check_install "$tmp/h1"; then
  report ok "manifest install -> $PRODUCT_BIN --version reports $version"
else
  report fail "manifest install"
fi

echo "== 2. install pinned WORKSHOP_VERSION=$version"
if (WORKSHOP_HOME="$tmp/h2" WORKSHOP_VERSION="$version" WORKSHOP_DOWNLOAD_BASE="$base/dl" sh "$install_sh") && check_install "$tmp/h2"; then
  report ok "pinned install verifies against SHA256SUMS"
else
  report fail "pinned install"
fi

echo "== 3. tampered checksum must be rejected"
if (WORKSHOP_HOME="$tmp/h3" WORKSHOP_MANIFEST_URL="$base/tampered.json" sh "$install_sh") 2>"$tmp/h3.err"; then
  report fail "tampered manifest was accepted"
elif grep -q "checksum mismatch" "$tmp/h3.err" && [[ ! -e "$tmp/h3/bin/$PRODUCT_BIN" ]]; then
  report ok "tampered checksum rejected, nothing installed"
else
  cat "$tmp/h3.err"
  report fail "tampered manifest failed for the wrong reason"
fi

echo "== 4. non-https manifest URL must be refused"
if (WORKSHOP_HOME="$tmp/h4" WORKSHOP_MANIFEST_URL="http://example.invalid/stable.json" sh "$install_sh") 2>"$tmp/h4.err"; then
  report fail "non-https URL was accepted"
elif grep -q "non-https" "$tmp/h4.err"; then
  report ok "non-https URL refused"
else
  cat "$tmp/h4.err"
  report fail "non-https URL failed for the wrong reason"
fi

echo
echo "smoke: $pass passed, $fail failed"
((fail == 0))
