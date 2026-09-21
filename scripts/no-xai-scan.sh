#!/usr/bin/env bash
# Workshop no-xAI / no-theft scan. Exit non-zero on any violation.
#
#   scripts/no-xai-scan.sh                           sources, plus the built binary when target/{debug,release}/workshop exists
#   scripts/no-xai-scan.sh --sources                 scan default-path sources (fast; runs on every PR)
#   scripts/no-xai-scan.sh --binary target/debug/workshop
#                                                    count forbidden strings in the built binary and compare
#                                                    against scripts/no-xai-binary-baseline.txt (regression = fail)
#   scripts/no-xai-scan.sh --binary BIN --write-baseline
#                                                    rewrite the baseline after a reviewed, justified change
#
# The source scan is deliberately narrow: it checks the compile-time defaults that first run hits
# (docs/workshop-production-plan.md section 1, gates 1-4) plus the token-theft markers (gate:no-theft).
# Comments, changelog, LICENSE and the optional xAI provider are allowlisted by construction.
set -uo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"
fail=0
violation() { printf 'VIOLATION: %s\n' "$*" >&2; fail=1; }
ok() { printf 'ok: %s\n' "$*"; }

# Extract the body of `impl Default for GrokComConfig` (up to the next `impl `/`fn ` at column 0 or `}` at column 0).
default_impl_body() {
  awk '/^impl Default for GrokComConfig/{f=1} f{print} f&&/^}/{exit}' "$1"
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

  # Telemetry: no compile-time bake-in anywhere in CI config.
  if grep -rEq '^[[:space:]]*(export[[:space:]]+)?GROK_TELEMETRY_BUILD_[A-Z_]+[[:space:]]*[:=]' .github/ scripts/ 2>/dev/null; then
    violation "CI sets GROK_TELEMETRY_BUILD_* (telemetry bake-in)"
  else ok "no GROK_TELEMETRY_BUILD_* assignment in CI or scripts"; fi

  # gate:no-theft — foreign OAuth / keychain / auth-file markers must not exist in production Rust.
  # Test fixtures may name a forbidden string as input to prove it is stripped (e.g. the adapters
  # env test showing ANTHROPIC_BASE_URL=127.0.0.1:3456 being dropped), so `*test*` files, the gates
  # crate, and full-line comments are excluded — the same rule the integration_gates fs audit uses.
  local theft_re='Claude Code-credentials|\.codex/auth\.json|\.cursor/sdk/auth\.json|share/opencode/auth\.json|opencode-with-claude|127\.0\.0\.1:3456|provider_autodock'
  local hits
  hits="$(grep -rEn --include='*.rs' "$theft_re" crates/ 2>/dev/null \
    | grep -v '^crates/workshop-gates/' \
    | grep -Ev '/[^:]*test[^:]*\.rs:' \
    | grep -Ev '^[^:]+:[0-9]+:[[:space:]]*(//|\*|///)' || true)"
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
NEEDLES=(auth.x.ai accounts.x.ai cli-chat-proxy.grok.com x.ai/cli @xai-official grok-build-public-artifacts xai-org-shared/grok-build api.mixpanel.com)
# Needles that must be zero in the binary regardless of baseline (fully replaced surfaces: updater).
# `x.ai/cli` is baseline-tracked instead: the remaining occurrences are embedded end-user docs
# (xai-grok-pager/docs/user-guide/01-getting-started.md, xai-grok-shell/README.md) that no code path
# fetches; the milestone B doc rebrand takes them to 0 and the baseline ratchets down with it.
ZERO_NEEDLES=(@xai-official grok-build-public-artifacts xai-org-shared/grok-build)

scan_binary() {
  local bin="$1" write="${2:-}"
  [ -f "$bin" ] || { violation "binary not found: $bin"; return; }
  local tmp; tmp="$(mktemp)"
  strings -a -n 6 "$bin" > "$tmp"
  local report=""
  for n in "${NEEDLES[@]}"; do
    local c; c="$(grep -F -c -- "$n" "$tmp" || true)"
    report+="$n $c"$'\n'
    printf 'binary: %-32s %s\n' "$n" "$c"
  done
  rm -f "$tmp"
  if [ "$write" = "--write-baseline" ]; then
    printf '%s' "$report" > "$BASELINE"; ok "wrote $BASELINE"; return
  fi
  for n in "${ZERO_NEEDLES[@]}"; do
    local c; c="$(printf '%s' "$report" | awk -v n="$n" '$1==n{print $2}')"
    [ "$c" = "0" ] || violation "binary contains '$n' ($c occurrences); must be 0"
  done
  if [ -f "$BASELINE" ]; then
    while read -r n base; do
      [ -z "$n" ] && continue
      local c; c="$(printf '%s' "$report" | awk -v n="$n" '$1==n{print $2}')"
      if [ "${c:-0}" -gt "$base" ]; then
        violation "binary: '$n' count $c exceeds reviewed baseline $base (regression)"
      elif [ "${c:-0}" -lt "$base" ]; then
        echo "note: '$n' dropped to $c (baseline $base); run --write-baseline to ratchet down" >&2
      fi
    done < "$BASELINE"
    ok "binary counts within $BASELINE"
  else
    violation "no $BASELINE; run with --write-baseline after review"
  fi
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
  *) sed -n '2,13p' "$0"; exit 64 ;;
esac
[ "$fail" = 0 ] && { echo "no-xai-scan: PASS"; exit 0; } || { echo "no-xai-scan: FAIL" >&2; exit 1; }
