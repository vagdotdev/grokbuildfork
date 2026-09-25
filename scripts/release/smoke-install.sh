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
# rejected, non-https manifest URL is refused. When <dist> also holds the voice assets
# (voice-engine-<version>-<platform>.tar.gz, MODEL.lock.json, ggml-*.bin) the install cases run
# with WORKSHOP_VOICE_TIER=base (which opts in to voice) and verify the helper and model, and
# further cases prove: a re-run transfers no model bytes, a corrupted model is replaced, a
# killed download resumes from its .partial, a 404 mirror falls back to the second source, and
# both sources failing installs nothing and exits 1. Always: the default install (no voice
# opt-in) downloads only `workshop` with product-style output, a re-run says "already
# installed", a plain file from a manual tar install is replaced, and an older install is
# reported as "Updated Workshop <old> → <new>", the PATH line lands once in the rc file of the
# user's shell (zsh, fish, bash; $HOME-relative for the default home; an older block is replaced).
# Every run gets its own HOME so no real shell rc is touched. Requires python3 (http.server).

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
voice=false
engine_asset="voice-engine-$version-$platform.tar.gz"
if [[ -f "$dist/$engine_asset" && -f "$dist/MODEL.lock.json" ]]; then
  voice=true
  cp "$dist/$engine_asset" "$dist/MODEL.lock.json" "$dist"/ggml-*.bin "$www/dl/v$version/" 2>/dev/null || die "voice assets incomplete in $dist"
  # The smallest tier is what CPU runners end up with; the smoke pins it to keep the run bounded.
  # A forced tier also opts in to installing voice now (the default install has no voice at all).
  export WORKSHOP_VOICE_TIER=${WORKSHOP_VOICE_TIER:-base}
  base_file=$(jq -r '.models.base.file' "$dist/MODEL.lock.json")
  base_sha=$(jq -r '.models.base.sha256' "$dist/MODEL.lock.json")
  base_size=$(jq -r '.models.base.size' "$dist/MODEL.lock.json")
  [[ -f "$www/dl/v$version/$base_file" ]] || die "voice smoke needs $base_file in $dist"
  # A second, "upstream" copy so the fallback path stays on the loopback server.
  mkdir -p "$www/upstream"
  cp "$www/dl/v$version/$base_file" "$www/upstream/"
  export WORKSHOP_VOICE_UPSTREAM_BASE="$base/upstream"
else
  export WORKSHOP_VOICE_SKIP=1
  echo "note: no voice assets in $dist; voice cases skipped"
fi

"$here/manifest.sh" --version "$version" --tag "v$version" --channel "$channel" --repo "$repo" \
  --dist "$dist" --out "$www/$channel.json" --asset-base "$base/dl/v$version" --attested false
jq --arg p "$platform" '.artifacts[$p].sha256 = ("0" * 64)' "$www/$channel.json" >"$www/tampered.json"

server_log="$tmp/http.log"
# Range-capable static server (python's http.server ignores Range, which the resume case needs).
(exec python3 "$here/range-httpd.py" "$port" "$www" 2>"$server_log") &
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
  $voice || return 0
  check_voice "$home"
}

check_voice() { # check_voice HOME -> helper runs, model present with the pinned sha, selection recorded
  local home=$1 out
  [[ -x "$home/bin/voice-engine" ]] || { echo "  no voice-engine at $home/bin/voice-engine"; return 1; }
  out=$("$home/bin/voice-engine" --version 2>&1) || { echo "  voice-engine --version failed: $out"; return 1; }
  [[ -f "$home/voice/$base_file" ]] || { echo "  model missing: $home/voice/$base_file"; return 1; }
  [[ "$(file_size "$home/voice/$base_file")" == "$base_size" ]] || { echo "  model size wrong"; return 1; }
  [[ "$(sha256_file "$home/voice/$base_file")" == "$base_sha" ]] || { echo "  model sha256 wrong"; return 1; }
  [[ "$(tr -d '[:space:]' <"$home/voice/model.selected")" == base ]] || { echo "  model.selected is not base"; return 1; }
  [[ ! -e "$home/voice/$base_file.partial" ]] || { echo "  stray .partial left behind"; return 1; }
  echo "  $out; model $base_file verified; tier $(cat "$home/voice/model.selected")"
}

model_gets() { grep -c "GET /dl/v$version/$base_file " "$server_log" || true; }

echo "== 1. install from $channel manifest"
if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h1" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") && check_install "$tmp/h1"; then
  report ok "manifest install -> $PRODUCT_BIN --version reports $version"
else
  report fail "manifest install"
fi

echo "== 2. install pinned WORKSHOP_VERSION=$version"
if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h2" WORKSHOP_VERSION="$version" WORKSHOP_DOWNLOAD_BASE="$base/dl" sh "$install_sh") && check_install "$tmp/h2"; then
  report ok "pinned install verifies against SHA256SUMS"
else
  report fail "pinned install"
fi

echo "== 3. tampered checksum must be rejected"
if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h3" WORKSHOP_MANIFEST_URL="$base/tampered.json" sh "$install_sh") 2>"$tmp/h3.err"; then
  report fail "tampered manifest was accepted"
elif grep -q "checksum mismatch" "$tmp/h3.err" && [[ ! -e "$tmp/h3/bin/$PRODUCT_BIN" ]]; then
  report ok "tampered checksum rejected, nothing installed"
else
  cat "$tmp/h3.err"
  report fail "tampered manifest failed for the wrong reason"
fi

echo "== 4. non-https manifest URL must be refused"
if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h4" WORKSHOP_MANIFEST_URL="http://example.invalid/stable.json" sh "$install_sh") 2>"$tmp/h4.err"; then
  report fail "non-https URL was accepted"
elif grep -q "non-https" "$tmp/h4.err"; then
  report ok "non-https URL refused"
else
  cat "$tmp/h4.err"
  report fail "non-https URL failed for the wrong reason"
fi

if $voice; then
  echo "== 5. voice: re-run transfers no model bytes"
  before=$(model_gets)
  if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h1" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h5.err"     && grep -q "Voice model already present" "$tmp/h5.err" && [[ "$(model_gets)" == "$before" ]]; then
    report ok "re-run says already present and made no model request (server log)"
  else
    cat "$tmp/h5.err"; report fail "re-run downloaded the model again or did not say so"
  fi

  echo "== 6. voice: corrupted model (one byte flipped) is replaced"
  printf 'ÿ' | dd of="$tmp/h1/voice/$base_file" bs=1 seek=1000 count=1 conv=notrunc 2>/dev/null
  if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h1" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h6.err"     && grep -q "failed verification; replacing" "$tmp/h6.err" && check_voice "$tmp/h1"; then
    report ok "corrupt model replaced with a verified copy"
  else
    cat "$tmp/h6.err"; report fail "corrupt model not replaced"
  fi

  echo "== 7. voice: interrupted download resumes from .partial"
  mkdir -p "$tmp/h7/voice"
  head -c 1000000 "$www/dl/v$version/$base_file" >"$tmp/h7/voice/$base_file.partial"
  before=$(model_gets)
  if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h7" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h7.err"     && check_voice "$tmp/h7" && grep -q "GET /dl/v$version/$base_file 206 " "$server_log"; then
    report ok "partial resumed (HTTP 206) and verified"
  else
    cat "$tmp/h7.err"; report fail "resume from .partial"
  fi

  echo "== 8. voice: mirror 404 falls back to the second source"
  if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h8" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" WORKSHOP_VOICE_MODEL_BASE="$base/nowhere" sh "$install_sh") 2>"$tmp/h8.err"     && check_voice "$tmp/h8" && grep -q "download failed from $base/nowhere" "$tmp/h8.err"; then
    report ok "mirror 404 -> upstream fallback -> verified model"
  else
    cat "$tmp/h8.err"; report fail "mirror fallback"
  fi

  echo "== 9. voice: both sources failing installs no model and exits non-zero"
  if (HOME="$tmp/home" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h9" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" WORKSHOP_VOICE_MODEL_BASE="$base/nowhere" WORKSHOP_VOICE_UPSTREAM_BASE="$base/nowhere-either" sh "$install_sh") 2>"$tmp/h9.err"; then
    report fail "install succeeded without a model"
  elif grep -q "could not download the voice model after 3 attempts" "$tmp/h9.err" && [[ ! -e "$tmp/h9/voice/$base_file" ]]; then
    report ok "both sources down: exit 1, no model file, re-run instruction printed"
  else
    cat "$tmp/h9.err"; report fail "double failure exited for the wrong reason"
  fi

fi

# The default install (no WORKSHOP_VOICE, no tier) downloads the one `workshop` archive and
# nothing else: no voice helper, no model, not even their checksum/pin files. Its output reads
# like a product and ends with the next command.
voice_gets() { grep -c -E "GET /dl/v$version/(voice-engine-|MODEL\.lock\.json|SHA256SUMS)" "$server_log" || true; }
check_cli() { # check_cli HOME -> the symlink layout and --version, voice not required
  local home=$1 out link
  [[ -L "$home/bin/$PRODUCT_BIN" ]] || { echo "  no symlink at $home/bin/$PRODUCT_BIN"; return 1; }
  link=$(readlink "$home/bin/$PRODUCT_BIN")
  [[ "$link" == "../downloads/$(versioned_bin_name "$version" "$platform")" ]] || { echo "  unexpected symlink target: $link"; return 1; }
  out=$("$home/bin/$PRODUCT_BIN" --version 2>&1) || { echo "  --version failed: $out"; return 1; }
  [[ "$(tr -d '[:space:]' <"$home/installed-version")" == "$version" ]] || { echo "  installed-version stamp missing or wrong"; return 1; }
  echo "  $out"
}

echo "== 10. default install: only workshop, no voice bytes, product-style output, next command"
before_voice=$(voice_gets)
# The PATH line goes into the rc file of the user's shell (zsh here), between markers, once; the
# output says so and ends with the next step. WORKSHOP_HOME is outside HOME, so the line carries
# the absolute directory.
rc_block_count() { grep -c -F '# >>> workshop installer >>>' "$1" 2>/dev/null || true; }
if (env -u WORKSHOP_VOICE_TIER HOME="$tmp/home10" SHELL=/bin/zsh WORKSHOP_HOME="$tmp/h10" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h10.err" \
  && check_cli "$tmp/h10" \
  && [[ ! -e "$tmp/h10/bin/voice-engine" ]] && [[ ! -e "$tmp/h10/voice" ]] \
  && [[ "$(voice_gets)" == "$before_voice" ]] \
  && grep -q "^Installed Workshop $version\.$" "$tmp/h10.err" \
  && grep -q "^Verifying… done$" "$tmp/h10.err" && grep -q "^Installing… done$" "$tmp/h10.err" \
  && ! grep -q -E '^\[[0-9]/[0-9]\]|^workshop: ' "$tmp/h10.err" \
  && grep -q "^Added $tmp/h10/bin to your PATH in ~/.zshrc\.$" "$tmp/h10.err" \
  && grep -q -F "  Open a new terminal and type  $PRODUCT_BIN" "$tmp/h10.err" \
  && grep -q -F "first run:  export PATH=\"$tmp/h10/bin:\$PATH\"" "$tmp/h10.err" \
  && [[ "$(rc_block_count "$tmp/home10/.zshrc")" == 1 ]] \
  && grep -q -x -F "export PATH=\"$tmp/h10/bin:\$PATH\"" "$tmp/home10/.zshrc" \
  && [[ ! -e "$tmp/home10/.bashrc" ]]; then
  report ok "only workshop installed; no helper/model/pin requests; 'Installed Workshop $version.'; PATH line written to ~/.zshrc once; next step printed"
else
  cat "$tmp/h10.err"; cat "$tmp/home10/.zshrc" 2>/dev/null; report fail "default install fetched voice assets, the output is not the product copy, or the PATH line was not written"
fi

echo "== 11. re-running the one-liner over the same version says so, keeps the layout and adds no second PATH block"
if (env -u WORKSHOP_VOICE_TIER HOME="$tmp/home10" SHELL=/bin/zsh WORKSHOP_HOME="$tmp/h10" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h11.err" \
  && check_cli "$tmp/h10" && grep -q "^Workshop $version was already installed; refreshed\.$" "$tmp/h11.err" \
  && [[ "$(rc_block_count "$tmp/home10/.zshrc")" == 1 ]] \
  && [[ "$(grep -c -F "$tmp/h10/bin" "$tmp/home10/.zshrc")" == 1 ]]; then
  report ok "re-run: 'already installed; refreshed', symlink intact, PATH block still exactly once"
else
  cat "$tmp/h11.err"; cat "$tmp/home10/.zshrc" 2>/dev/null; report fail "re-run over the same version"
fi

echo "== 12. a manual tar install (plain file at bin/workshop) is replaced cleanly"
mkdir -p "$tmp/h12/bin" "$tmp/x12"
tar -xzf "$dist/$asset" -C "$tmp/x12"
cp "$tmp/x12/$PRODUCT_BIN" "$tmp/h12/bin/$PRODUCT_BIN"
chmod 755 "$tmp/h12/bin/$PRODUCT_BIN"
if (env -u WORKSHOP_VOICE_TIER HOME="$tmp/home12" SHELL=/usr/bin/fish WORKSHOP_HOME="$tmp/h12" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h12.err" \
  && check_cli "$tmp/h12" && grep -q "^Replaced the existing Workshop with $version\.$" "$tmp/h12.err" \
  && grep -q -x -F "fish_add_path $tmp/h12/bin" "$tmp/home12/.config/fish/config.fish" \
  && grep -q -F "first run:  fish_add_path $tmp/h12/bin" "$tmp/h12.err"; then
  report ok "plain file replaced by the versioned symlink; 'Replaced the existing Workshop with $version.'; fish gets fish_add_path in config.fish"
else
  cat "$tmp/h12.err"; report fail "manual tar install was not replaced cleanly, or the fish PATH line is missing"
fi

echo "== 13. an older installer-made install is updated and says from which version"
mkdir -p "$tmp/h13/bin"
ln -s "../downloads/$(versioned_bin_name 0.0.1 "$platform")" "$tmp/h13/bin/$PRODUCT_BIN"
if (env -u WORKSHOP_VOICE_TIER HOME="$tmp/home13" SHELL=/bin/bash WORKSHOP_HOME="$tmp/h13" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h13.err" \
  && check_cli "$tmp/h13" && grep -q "^Updated Workshop 0\.0\.1 → $version\.$" "$tmp/h13.err" \
  && grep -q -x -F "export PATH=\"$tmp/h13/bin:\$PATH\"" "$tmp/home13/.bashrc"; then
  report ok "'Updated Workshop 0.0.1 → $version.' and the new symlink in place; bash gets the line in ~/.bashrc"
else
  cat "$tmp/h13.err"; report fail "update over an older install"
fi

echo "== 14. the default home under \$HOME is written as \$HOME, and an older PATH block is replaced, not duplicated"
mkdir -p "$tmp/home14"
printf 'PRE\n# >>> workshop installer >>>\nexport PATH="/old/workshop/bin:%s"\n# <<< workshop installer <<<\nPOST\n' "\$PATH" >"$tmp/home14/.bashrc"
if (env -u WORKSHOP_VOICE_TIER HOME="$tmp/home14" SHELL=/bin/bash WORKSHOP_HOME="$tmp/home14/.workshop" WORKSHOP_CHANNEL="$channel" WORKSHOP_MANIFEST_URL="$base/$channel.json" sh "$install_sh") 2>"$tmp/h14.err" \
  && check_cli "$tmp/home14/.workshop" \
  && grep -q -x -F "export PATH=\"\$HOME/.workshop/bin:\$PATH\"" "$tmp/home14/.bashrc" \
  && ! grep -q -F '/old/workshop/bin' "$tmp/home14/.bashrc" \
  && [[ "$(rc_block_count "$tmp/home14/.bashrc")" == 1 ]] \
  && grep -q -x PRE "$tmp/home14/.bashrc" && grep -q -x POST "$tmp/home14/.bashrc" \
  && grep -q "^Added ~/.workshop/bin to your PATH in ~/.bashrc\.$" "$tmp/h14.err"; then
  report ok "\$HOME-relative line, old block replaced in place, the rest of the file untouched"
else
  cat "$tmp/h14.err"; cat "$tmp/home14/.bashrc"; report fail "PATH block replacement"
fi

echo
echo "smoke: $pass passed, $fail failed"
((fail == 0))
