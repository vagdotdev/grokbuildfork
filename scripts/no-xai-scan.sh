#!/usr/bin/env bash
# Forbidden-default scan for Workshop builds (plan §1 "How the gates are enforced").
#
#   scripts/no-xai-scan.sh [path/to/workshop-binary]
#
# 1. Default-path sources: the patched upstream files that hold compile-time
#    defaults must not name xAI infrastructure outside comments. (The cargo
#    gates assert the same on the compiled values; this is the cheap pre-check.)
# 2. Built binary: the xAI update channel, proxy, asset, relay, and gateway
#    literals must be absent from the release binary. `auth.x.ai` and
#    `accounts.x.ai` are allowed to appear: they back the optional, labeled
#    xAI card and are only reached after an explicit opt-in; their count is
#    printed so a review can spot growth.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
bin="${1:-target/release/workshop}"
fail=0

strip_comments() {
  # Drop // line comments and /* */ blocks (good enough for constants files).
  sed -E 's://.*$::' "$1" | perl -0pe 's:/\*.*?\*/::gs'
}

echo "== default-path sources"
default_sources=(
  crates/codegen/xai-grok-env/src/lib.rs
  crates/codegen/xai-grok-login/src/config.rs
  crates/codegen/xai-grok-update/src/version.rs
  crates/codegen/xai-grok-models/default_models.json
  crates/codegen/xai-grok-shell-base/src/env.rs
  crates/codegen/xai-dirs/src/lib.rs
)
forbidden_source_re='cli-chat-proxy\.grok\.com|assets\.grok\.com|code\.grok\.com|wss://grok\.com|https://grok\.com|x\.ai/cli|grok-build-public-artifacts|@xai-official/grok|xai-org-shared/grok-build|computer-hub\.grok\.com|"grok-4|api\.x\.ai'
for f in "${default_sources[@]}"; do
  if hits="$(strip_comments "$f" | grep -nE "$forbidden_source_re" || true)"; [[ -n "$hits" ]]; then
    # auth.x.ai / accounts.x.ai in login config are the optional provider; everything else is a fail.
    echo "FAIL $f"; echo "$hits" | sed 's/^/    /'; fail=1
  else
    echo "ok   $f"
  fi
done
# The login config may keep the xAI issuer only in the *optional* provider constructor, never in `impl Default`.
if awk '/^impl Default for GrokComConfig/,/^}/' crates/codegen/xai-grok-login/src/config.rs | grep -qE 'xai_oauth2_issuer\(\)|auth\.x\.ai'; then
  echo "FAIL crates/codegen/xai-grok-login/src/config.rs: GrokComConfig::default still bakes the xAI issuer"; fail=1
fi

echo "== built binary: $bin"
if [[ ! -x "$bin" ]]; then
  echo "skip (binary not built)"
else
  forbidden_bin=(
    'x.ai/cli'
    'grok-build-public-artifacts'
    '@xai-official/grok'
    'xai-org-shared/grok-build'
    'cli-chat-proxy.grok.com'
    'assets.grok.com'
    'code.grok.com/ws'
    'wss://grok.com'
    'computer-hub.grok.com'
  )
  for needle in "${forbidden_bin[@]}"; do
    n="$(strings -n 8 "$bin" | grep -cF -- "$needle" || true)"
    if [[ "$n" -gt 0 ]]; then echo "FAIL $needle ($n hits)"; fail=1; else echo "ok   $needle"; fi
  done
  for allowed in 'auth.x.ai' 'accounts.x.ai' 'api.x.ai'; do
    n="$(strings -n 8 "$bin" | grep -cF -- "$allowed" || true)"
    echo "info $allowed: $n hits (optional xAI card / provider only; review on growth)"
  done
fi

if [[ $fail -ne 0 ]]; then echo "no-xai scan: FAIL"; exit 1; fi
echo "no-xai scan: OK"
