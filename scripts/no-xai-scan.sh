#!/usr/bin/env bash
# Workshop no-xAI / no-theft scan. Exit non-zero on any violation.
#
#   scripts/no-xai-scan.sh                           sources, plus the built binary when target/{debug,release}/workshop exists
#   scripts/no-xai-scan.sh --sources                 scan default-path sources (fast; runs on every PR)
#   scripts/no-xai-scan.sh --binary target/debug/workshop
#                                                    every distinct string in the binary that names a forbidden
#                                                    host must be one of the reviewed contexts listed in
#                                                    scripts/no-xai-binary-baseline.txt (anything else = fail);
#                                                    the must-be-zero needles may not appear at all
#   scripts/no-xai-scan.sh --binary BIN --show       also print every matching string and which context
#                                                    covers it (for reviewing a baseline change)
#
# The binary check is independent of codegen units, LTO and section layout: `strings` output is
# de-duplicated (`sort -u`) and matched against *contexts* (substrings of the reviewed literals),
# not counted. Raw counts drift with every profile — the same literal is emitted once per codegen
# unit that inlines it, and adjacent non-NUL-terminated Rust literals fuse into one `strings`
# line with whatever neighbour the linker chose — so a count baseline written from a debug build
# fails a release build for no semantic reason. A regression here means a *new* string that
# names an xAI/Mixpanel host: add a context to the baseline only after reviewing where it comes from.
#
# The source scan is deliberately narrow: it checks the compile-time defaults that first run hits
# (docs/workshop-production-plan.md section 1, gates 1-4) plus the token-theft markers (gate:no-theft).
# Comments, changelog, LICENSE and the optional xAI provider are allowlisted by construction.
set -uo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 2
fail=0
violation() { printf 'VIOLATION: %s\n' "$*" >&2; fail=1; }
ok() { printf 'ok: %s\n' "$*"; }

# Extract the body of `impl Default for GrokComConfig` (up to the next `impl `/`fn ` at column 0 or `}` at column 0).
default_impl_body() {
  awk '/^impl Default for GrokComConfig/{f=1} f{print} f&&/^}/{exit}' "$1"
}

# Emit `path:lineno:code` for every production Rust line under crates/, with `//`/`///` line
# comments dropped and `#[cfg(test)]`-guarded modules removed by brace depth. Mirrors the cargo
# gate's `scannable_source` so the two share one definition of "production code". The gates crate
# and `*test*.rs` files are skipped (they legitimately name forbidden strings as fixtures).
scannable_rs_lines() {
  local f
  while IFS= read -r f; do
    case "$f" in
      crates/workshop-gates/*) continue ;;
      *test*.rs) continue ;;
    esac
    awk -v path="$f" '
      function count(s, ch,   n, i) { n=0; for(i=1;i<=length(s);i++) if(substr(s,i,1)==ch) n++; return n }
      {
        line=$0
        stripped=line; sub(/^[[:space:]]+/, "", stripped)
        # A #[cfg(test)] attribute arms a test region; it opens once the guarded item descends into
        # a brace (mod/fn on the next line), and closes when depth returns to the attribute depth.
        if (stripped ~ /^#\[cfg\(test\)\]/) { armed=1; base_depth=depth }
        is_comment = (stripped ~ /^\/\//) || (stripped ~ /^\*/)
        if (!armed && !in_test && !is_comment && stripped != "") print path ":" NR ":" line
        depth += count(line, "{") - count(line, "}")
        if (armed && depth > base_depth) { armed=0; in_test=1; test_depth=base_depth }
        else if (armed && depth == base_depth && stripped ~ /;[[:space:]]*$/) armed=0
        if (in_test && depth <= test_depth) in_test=0
      }
    ' "$f"
  done < <(find crates -name '*.rs' -type f | sort)
}

scan_sources() {
  # Gate 1: the default GrokComConfig must not construct the xAI issuer.
  local cfg=crates/codegen/xai-grok-login/src/config.rs
  if default_impl_body "$cfg" | grep -Eq 'xai_oauth2_issuer\(\)|XAI_OAUTH2_ISSUER|auth\.x\.ai|localhost:22255|b1a00492'; then
    violation "gate1: $cfg: GrokComConfig::default() still constructs the xAI OAuth2 provider"
  else ok "gate1: GrokComConfig::default() does not reference the xAI issuer"; fi

  # Gate 2: the interactive login method is pushed only behind an explicit OAuth2-provider flag.
  local am=crates/codegen/xai-grok-shell/src/agent/auth_method.rs
  if ! grep -q 'has_oauth2_provider' "$am"; then
    violation "gate2: $am: build_auth_methods has no has_oauth2_provider gate (grok.com advertised unconditionally)"
  else ok "gate2: build_auth_methods gates grok.com on has_oauth2_provider"; fi
  if grep -Eq 'unwrap_or\("grok\.com"\)' crates/codegen/xai-grok-pager/src/views/welcome/mod.rs; then
    violation "gate2: welcome screen still defaults the login label to grok.com"
  else ok "gate2: welcome screen has no grok.com login label default"; fi

  # Gate 3: compiled production endpoint set carries no grok.com / x.ai host.
  local env=crates/codegen/xai-grok-env/src/lib.rs
  if awk '/^const PRODUCTION_ENDPOINTS/{f=1} f{print} f&&/^};/{exit}' "$env" | grep -Eq 'grok\.com|x\.ai'; then
    violation "gate3: $env: PRODUCTION_ENDPOINTS still points at grok.com / x.ai"
  else ok "gate3: PRODUCTION_ENDPOINTS is neutral"; fi
  if grep -Eq '^pub const CLI_CHAT_PROXY_BASE_URL_DEFAULT: &str = "https://cli-chat-proxy\.grok\.com' crates/codegen/xai-grok-shell/src/agent/config.rs; then
    violation "gate3: shell CLI_CHAT_PROXY_BASE_URL_DEFAULT still hardcodes cli-chat-proxy.grok.com"
  else ok "gate3: shell proxy default derives from xai-grok-env"; fi

  # Gate 4: updater constants (internal/updater-repoint-spec.md). String literals only; comments may name what is forbidden.
  local upd=crates/codegen/xai-grok-update/src
  if grep -Eq '"https://x\.ai/cli|storage\.googleapis\.com/grok-build-public-artifacts|"@xai-official/grok|"xai-org-shared/grok-build' "$upd/version.rs" "$upd/auto_update.rs" "$upd/../build.rs"; then
    violation "gate4: xai-grok-update still points at x.ai/cli, the Grok GCS bucket, @xai-official/grok or xai-org-shared/grok-build"
  else ok "gate4: updater constants are not xAI channels"; fi
  if ! grep -q 'WORKSHOP_RELEASE_REPO' "$upd/../build.rs" 2>/dev/null; then
    violation "gate4: updater does not bake WORKSHOP_RELEASE_REPO (build.rs missing)"
  else ok "gate4: release repo comes from WORKSHOP_RELEASE_REPO"; fi
  if grep -Eq 'format!\("grok-\{' "$upd/auto_update.rs"; then
    violation "gate4: updater still writes grok-<version>-<platform> downloads (layout must be workshop-<version>-<platform>)"
  else ok "gate4: managed layout is workshop-<version>-<platform>"; fi
  if ! grep -q 'WORKSHOP_ENABLE_AUTOUPDATE' crates/codegen/xai-grok-pager-bin/src/main.rs; then
    violation "gate4: background auto-update is not gated off by WORKSHOP_ENABLE_AUTOUPDATE"
  else ok "gate4: background auto-update is off unless WORKSHOP_ENABLE_AUTOUPDATE"; fi

  # Default model / aux tools.
  local dm=crates/codegen/xai-grok-models/default_models.json
  if python3 - "$dm" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
bad=[k for k in ("default","web_search","image_description","session_summary") if str(d.get(k,"")).lower().startswith("grok")]
sys.exit(1 if bad else 0)
PY
  then ok "default model / aux tool models are not grok-*"; else violation "default_models.json: default/aux model is still grok-*"; fi

  # Identity.
  if ! grep -Eq '^name = "workshop"' crates/codegen/xai-grok-pager-bin/Cargo.toml; then
    violation "pager-bin [[bin]] name is not workshop"
  else ok "binary is named workshop"; fi
  if ! grep -q 'WORKSHOP_HOME' crates/codegen/xai-dirs/src/lib.rs; then
    violation "xai-dirs does not honor WORKSHOP_HOME"
  else ok "home resolves via WORKSHOP_HOME / ~/.workshop"; fi
  local sleep_rs=crates/codegen/xai-grok-pager/src/notifications/sleep.rs
  if grep -q -- '--who=grok' "$sleep_rs" || ! grep -q -- '--who=workshop' "$sleep_rs"; then
    violation "$sleep_rs: the systemd idle inhibitor does not identify as workshop (--who=)"
  else ok "systemd-inhibit registers as --who=workshop"; fi

  # Telemetry: no compile-time bake-in anywhere in CI config.
  if grep -rEq '^[[:space:]]*(export[[:space:]]+)?GROK_TELEMETRY_BUILD_[A-Z_]+[[:space:]]*[:=]' .github/ scripts/ 2>/dev/null; then
    violation "CI sets GROK_TELEMETRY_BUILD_* (telemetry bake-in)"
  else ok "no GROK_TELEMETRY_BUILD_* assignment in CI or scripts"; fi

  # gate:no-theft — foreign OAuth / keychain / auth-file markers must not exist in production Rust.
  # Comments and `#[cfg(test)]` modules are stripped first (same rule as the workshop-gates cargo
  # gate `scannable_source`), so a test fixture may still name a forbidden string as *input* to
  # prove it is dropped (e.g. workshop-adapters env test: ANTHROPIC_BASE_URL=127.0.0.1:3456). The
  # gates crate and `*test*.rs` files are excluded outright.
  local theft_re='Claude Code-credentials|\.codex/auth\.json|\.cursor/sdk/auth\.json|share/opencode/auth\.json|opencode-with-claude|127\.0\.0\.1:3456|provider_autodock'
  local hits
  hits="$(scannable_rs_lines | grep -En "$theft_re" || true)"
  if [ -n "$hits" ]; then
    violation "gate:no-theft markers found:"; printf '%s\n' "$hits" >&2
  else ok "gate:no-theft: no foreign-credential markers in crates/ (production code)"; fi
  if [ -e crates/codegen/xai-grok-pager/src/provider_autodock.rs ]; then
    violation "provider_autodock.rs is present (must not be ported)"
  fi
  # Workshop crates never read another app's auth files.
  # Code lines only: doc comments may name what is forbidden.
  hits="$(grep -rEn --include='*.rs' 'auth\.json|find-generic-password|keychain' crates/workshop-auth/src 2>/dev/null | grep -Ev '^[^:]+:[0-9]+:[[:space:]]*//' || true)"
  if [ -n "$hits" ]; then violation "workshop-auth touches auth files / keychain:"; printf '%s\n' "$hits" >&2
  else ok "workshop-auth has no auth-file / keychain access"; fi
}

BASELINE="scripts/no-xai-binary-baseline.txt"
NEEDLES=(auth.x.ai accounts.x.ai cli-chat-proxy.grok.com x.ai/cli @xai-official grok-build-public-artifacts xai-org-shared/grok-build api.mixpanel.com --who=grok)
# Needles that must be zero in the binary regardless of baseline (fully replaced surfaces: the
# updater channel, the xAI proxy host, and the x.ai/cli install/CDN paths — the embedded end-user
# docs that used to carry the last `x.ai/cli` and `cli-chat-proxy.grok.com` mentions were rebranded
# in the milestone B string patch, so any reappearance is a regression, not a baseline drift; the
# `systemd-inhibit --who=grok` lock name the process registers with the OS is branding too).
ZERO_NEEDLES=(@xai-official grok-build-public-artifacts xai-org-shared/grok-build x.ai/cli cli-chat-proxy.grok.com --who=grok)

# Printable ASCII runs of >= 6 bytes from the whole file. GNU binutils on Linux; Xcode CLT ships
# `strings` on macOS (llvm-strings underneath) with the same -a/-n flags; a plain Python fallback
# keeps the gate alive on a runner without either.
binary_strings() {
  if command -v strings >/dev/null 2>&1; then strings -a -n 6 "$1"
  elif command -v llvm-strings >/dev/null 2>&1; then llvm-strings -a -n 6 "$1"
  else
    python3 - "$1" <<'PY'
import re, sys
with open(sys.argv[1], 'rb') as f:
    for m in re.finditer(rb'[\x20-\x7e]{6,}', f.read()):
        sys.stdout.write(m.group().decode('ascii') + '\n')
PY
  fi
}

scan_binary() {
  local bin="$1" show="${2:-}"
  [ -f "$bin" ] || { violation "binary not found: $bin"; return; }
  [ -f "$BASELINE" ] || { violation "no $BASELINE (reviewed contexts); see the header of this script"; return; }
  local tmp; tmp="$(mktemp)"
  binary_strings "$bin" | LC_ALL=C sort -u > "$tmp"
  local n bn line frag rest covered distinct unreviewed zero
  for n in "${NEEDLES[@]}"; do
    zero=0; for bn in "${ZERO_NEEDLES[@]}"; do [ "$bn" = "$n" ] && zero=1; done
    # Reviewed contexts for this needle: `needle<TAB>context` lines of the baseline.
    local contexts=() seen=""
    while IFS=$'\t' read -r bn frag; do
      [ "$bn" = "$n" ] && [ -n "$frag" ] && contexts+=("$frag")
    done < <(grep -v '^[[:space:]]*#' "$BASELINE")
    distinct=0 unreviewed=0
    while IFS= read -r line; do
      distinct=$((distinct + 1))
      # Delete every reviewed context; a needle that survives is an unreviewed string.
      rest="$line" covered=""
      for frag in ${contexts[@]+"${contexts[@]}"}; do
        if [[ "$rest" == *"$frag"* ]]; then
          rest="${rest//"$frag"/}"; covered+="${covered:+ | }$frag"; seen+=$'\n'"$frag"
        fi
      done
      if [ "$zero" = 1 ] || [[ "$rest" == *"$n"* ]]; then
        unreviewed=$((unreviewed + 1))
        violation "binary: string names '$n'$([ "$zero" = 1 ] && echo ' (must be absent)' || echo " and no reviewed context in $BASELINE covers it"):"
        printf '    %s\n' "${line:0:240}" >&2
      elif [ "$show" = "--show" ]; then
        printf '    [%s] %s\n' "$covered" "${line:0:200}"
      fi
    done < <(grep -F -- "$n" "$tmp" || true)
    printf 'binary: %-30s distinct strings %-3s unreviewed %s\n' "$n" "$distinct" "$unreviewed"
    # Ratchet down: a reviewed context that no longer occurs should leave the baseline.
    for frag in ${contexts[@]+"${contexts[@]}"}; do
      case "$seen" in *$'\n'"$frag"*) ;; *) echo "note: reviewed context for '$n' no longer in the binary; drop it from $BASELINE: $frag" >&2 ;; esac
    done
  done
  rm -f "$tmp"
  [ "$fail" = 0 ] && ok "binary: every forbidden-host string is a reviewed context in $BASELINE; must-be-zero needles absent"
}

case "${1:-}" in
  --sources) scan_sources ;;
  --binary) scan_binary "${2:?binary path}" "${3:-}" ;;
  "")
    # Bare invocation (scripts/sync/verify.sh): sources always; the binary when a build is present.
    scan_sources
    for bin in target/debug/workshop target/release/workshop; do
      if [ -f "$bin" ]; then scan_binary "$bin"; break; fi
    done
    ;;
  *) sed -n '2,12p' "$0"; exit 64 ;;
esac
if [ "$fail" = 0 ]; then echo "no-xai-scan: PASS"; exit 0; fi
echo "no-xai-scan: FAIL" >&2
exit 1
