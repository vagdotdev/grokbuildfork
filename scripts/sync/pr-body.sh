#!/usr/bin/env bash
# Turn $SYNC_REPORT_DIR into the sync PR: verdict, title, labels, body.
#
#   scripts/sync/pr-body.sh [--report-dir DIR]
#
# Writes verdict (green|attention|red), pr-title.txt, labels.txt, pr-body.md
# and prints the body. Verdict rules (docs/upstream-sync.md):
#   red        a gate:* patch failed to apply, the series is invalid, upstream
#              collides with an overlay path, the build failed, or a gate suite
#              failed. Never merge.
#   attention  a product/branding patch was deferred, a patch became a noop,
#              a security-review file changed upstream, stale files were
#              dropped, or the gates could not be run. Draft PR.
#   green      everything applied, built, and the gates passed. Ready for
#              human review (auto-PR is not auto-merge).
# shellcheck disable=SC2016,SC2034  # backticks are Markdown; tsv columns are read positionally
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

cd "$(repo_root)"
init_report_dir
report_load
[[ -n "${UPSTREAM_SHA:-}" ]] || die "sync.env has no UPSTREAM_SHA; run fetch-upstream.sh first"

R="$SYNC_REPORT_DIR"
tsv="$R/patches.tsv"
[[ -f "$tsv" ]] || : > "$tsv"

count_status() { awk -F'\t' -v s="$1" '$3 == s' "$tsv" | wc -l | tr -d ' '; }
n_lines() { [[ -f "$1" ]] && wc -l < "$1" | tr -d ' ' || echo 0; }

# --- verdict ----------------------------------------------------------------
red_reasons=() attention_reasons=()
(( ${REPLAY_RED:-0} )) && red_reasons+=("a gate patch failed to apply or the series is invalid (${PATCH_FAILED_GATE:-0} gate failure(s), ${PATCH_LINT_ERRORS:-0} series error(s))")
(( ${OVERLAY_COLLISION_COUNT:-0} )) && red_reasons+=("upstream now ships ${OVERLAY_COLLISION_COUNT} file(s) under overlay paths (rule 4: rename the Workshop path)")
[[ "${VERIFY_BUILD:-}" == fail ]] && red_reasons+=("cargo check failed")
for g in GATE_TESTS NO_XAI_SCAN ADAPTER_TESTS; do
  v="VERIFY_$g"
  [[ "${!v:-}" == fail ]] && red_reasons+=("$g failed")
done
(( ${PATCH_DEFERRED:-0} )) && attention_reasons+=("${PATCH_DEFERRED} product/branding patch(es) deferred; TODO(sync) entries below must be resolved or accepted")
(( ${PATCH_NOOP:-0} )) && attention_reasons+=("${PATCH_NOOP} patch(es) are already contained upstream; drop them from the series")
[[ -s "$R/security-review.txt" ]] && attention_reasons+=("upstream changed $(n_lines "$R/security-review.txt") file(s) on the security-review list (rule 7)")
(( ${DROPPED_FILE_COUNT:-0} )) && attention_reasons+=("${DROPPED_FILE_COUNT} non-upstream, non-overlay file(s) dropped by the tree replacement")
if [[ "${VERIFY_BUILD:-skipped}" == skipped || "${VERIFY_SKIPPED:-0}" == 1 ]]; then
  attention_reasons+=("build/gates were not run (${VERIFY_SKIPPED_REASON:-skipped})")
elif [[ "${VERIFY_GATES_PRESENT:-0}" == 0 ]]; then
  attention_reasons+=("no gate suite exists on this branch yet, so the no-xAI gates are unproven")
fi
[[ "${VERIFY_DIRTY:-0}" == 1 ]] && attention_reasons+=("verification left tracked files modified")

if (( ${#red_reasons[@]} )); then verdict=red
elif (( ${#attention_reasons[@]} )); then verdict=attention
else verdict=green; fi

short_new="$(short_sha "$UPSTREAM_SHA")"
title="chore(sync): grok-build $short_new (${UPSTREAM_VERSION:-unknown version})"
labels=(upstream-sync)
[[ $verdict == red ]] && labels+=(sync-red)
[[ $verdict == attention ]] && labels+=(sync-needs-attention)
[[ -s "$R/security-review.txt" ]] && labels+=(security-review)

printf '%s\n' "$verdict" > "$R/verdict"
printf '%s\n' "$title" > "$R/pr-title.txt"
printf '%s\n' "${labels[@]}" > "$R/labels.txt"
report_set VERDICT "$verdict"
report_set PR_TITLE "$title"

# --- body -------------------------------------------------------------------
lock_short="${LOCK_SHA:+$(short_sha "$LOCK_SHA")}"
run_url=""
[[ -n "${GITHUB_SERVER_URL:-}" && -n "${GITHUB_REPOSITORY:-}" && -n "${GITHUB_RUN_ID:-}" ]] \
  && run_url="$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID"
upstream_web="${UPSTREAM_URL%.git}"

status_cell() { # STATUS
  case "$1" in
    applied) printf 'applied' ;;
    applied-3way) printf 'applied (3-way, patch refreshed)' ;;
    noop) printf 'noop (already upstream)' ;;
    failed) printf '**FAILED — gate patch**' ;;
    deferred) printf '**deferred** (non-gate)' ;;
    missing) printf '**missing file**' ;;
    disabled) printf '**gate patch commented out**' ;;
    *) printf '%s' "$1" ;;
  esac
}

{
  printf '<!-- workshop-sync:report -->\n'
  printf '## Upstream sync: grok-build `%s` (%s)\n\n' "$short_new" "${UPSTREAM_VERSION:-?}"
  if [[ "${UPSTREAM_MOVED:-1}" == 0 ]]; then
    printf '_Forced replay of the locked snapshot: upstream did not move, so there is nothing to sync and no PR is opened. This run is a replay proof; the verdict and verification below are its result._\n\n'
  fi
  case $verdict in
    red)
      printf '**Verdict: RED — do not merge.**\n\n'
      for r in "${red_reasons[@]}"; do printf -- '- %s\n' "$r"; done
      printf '\nGate patches are never skipped or commented out to go green. Refresh the failing patch on this branch (see *How to fix* below), then re-run the sync.\n\n'
      ;;
    attention)
      printf '**Verdict: needs attention** (draft until a human resolves the points below).\n\n'
      for r in "${attention_reasons[@]}"; do printf -- '- %s\n' "$r"; done
      printf '\n'
      ;;
    green)
      printf '**Verdict: green.** Patch series replayed, overlay + pager-bin built, gate suites passed. Human review is still required; auto-PR is not auto-merge.\n\n'
      ;;
  esac

  printf '| | locked | new |\n|---|---|---|\n'
  printf '| upstream commit | `%s` | [`%s`](%s/commit/%s) |\n' "${lock_short:-none}" "$short_new" "$upstream_web" "$UPSTREAM_SHA"
  printf '| `SOURCE_REV` (monorepo, informational) | `%s` | `%s` |\n' "${LOCK_SOURCE_REV:-?}" "${UPSTREAM_SOURCE_REV:-?}"
  printf '| version (`xai-grok-pager-bin`) | %s | %s |\n' "${LOCK_VERSION:-?}" "${UPSTREAM_VERSION:-?}"
  printf '| date | fetched %s | committed %s, fetched %s |\n\n' "${LOCK_FETCHED_AT:-?}" "${UPSTREAM_COMMIT_DATE:-?}" "${LOCK_UPDATED_AT:-$(date -u +%Y-%m-%d)}"

  n_commits=$(n_lines "$R/upstream-commits.txt")
  if (( n_commits )); then
    printf '### Upstream commits (%s)\n\n' "$n_commits"
    head -n 25 "$R/upstream-commits.txt" | sed 's/^/- /'
    (( n_commits > 25 )) && printf -- '- … %s more\n' "$((n_commits - 25))"
    printf '\n'
  fi

  if [[ -s "$R/security-review.txt" ]]; then
    n_sec=$(n_lines "$R/security-review.txt")
    printf '### Security review required\n\nUpstream changed %s file(s) on `scripts/sync/security-review-paths.txt` (login / OIDC / endpoints / updater / paths / telemetry). Treat this sync as a security review, not a routine refresh (conflict policy rule 7):\n\n' "$n_sec"
    if (( n_sec > 15 )); then
      head -n 15 "$R/security-review.txt" | sed 's/^/- `/; s/$/`/'
      printf '\n<details><summary>%s more</summary>\n\n' "$((n_sec - 15))"
      tail -n +16 "$R/security-review.txt" | sed 's/^/- `/; s/$/`/'
      printf '\n</details>\n'
    else
      sed 's/^/- `/; s/$/`/' "$R/security-review.txt"
    fi
    printf '\n'
  fi

  printf '### Patch replay (%s in series)\n\n' "${PATCH_TOTAL:-0}"
  if [[ "${SERIES_PRESENT:-0}" == 0 ]]; then
    printf '_No `patches/series` on this branch; nothing was replayed._\n\n'
  elif [[ -s "$tsv" ]]; then
    printf '| # | patch | tags | result | files changed upstream |\n|---|---|---|---|---|\n'
    i=0
    while IFS=$'\t' read -r name tags status mode note files changed; do
      i=$((i + 1))
      ch="—"
      if [[ -n "$changed" ]]; then ch="$(printf '%s' "$changed" | tr ',' '\n' | sed 's/.*/`&`/' | paste -sd' ' -)"; fi
      printf '| %s | `%s` | %s | %s | %s |\n' "$i" "$name" "${tags:-—}" "$(status_cell "$status")" "$ch"
    done < "$tsv"
    printf '\n'
  else
    printf '_Series is empty._\n\n'
  fi

  if (( $(count_status failed) + $(count_status missing) + $(count_status disabled) )); then
    printf '#### Gate patches that did not apply\n\n'
    while IFS=$'\t' read -r name tags status mode note files changed; do
      case "$status" in
        failed) cat "$R/patch-detail/$name.md" 2>/dev/null || printf '#### `%s` — failed\n\n' "$name" ;;
        missing) printf '#### `%s` — listed in `series` but the file is missing.\n\n' "$name" ;;
        disabled) printf '#### `%s` — commented out of `series` while tagged `%s`. Gate patches are refreshed, never disabled.\n\n' "$name" "$tags" ;;
      esac
    done < "$tsv"
  fi

  if (( $(count_status deferred) )); then
    printf '#### Deferred non-gate patches\n\nEach line must be resolved (refresh the patch) or explicitly accepted by a reviewer before merge. Deferral is not allowed if it re-enables xAI login, endpoints, or the updater.\n\n'
    while IFS=$'\t' read -r name tags status mode note files changed; do
      [[ "$status" == deferred ]] || continue
      printf -- '- TODO(sync): `%s` [%s] deferred — user-visible gap: %s\n' "$name" "$tags" "${note:-_not described in series; describe before merge_}"
    done < "$tsv"
    printf '\n'
    while IFS=$'\t' read -r name tags status mode note files changed; do
      [[ "$status" == deferred ]] && cat "$R/patch-detail/$name.md" 2>/dev/null
    done < "$tsv"
  fi

  if (( $(count_status applied-3way) + $(count_status noop) )); then
    printf '#### Refreshed / noop patches\n\n'
    while IFS=$'\t' read -r name tags status mode note files changed; do
      [[ "$status" == applied-3way || "$status" == noop ]] && cat "$R/patch-detail/$name.md" 2>/dev/null
    done < "$tsv"
  fi

  printf '### Verification\n\n| check | result |\n|---|---|\n'
  printf '| `%s` | %s |\n' "${VERIFY_BUILD_CMD:-cargo check -p xai-grok-pager-bin}" "${VERIFY_BUILD:-not run}"
  printf '| `cargo test -p workshop-gates` (Gates 1–4, no-theft) | %s |\n' "${VERIFY_GATE_TESTS:-not run}"
  printf '| `scripts/no-xai-scan.sh` | %s |\n' "${VERIFY_NO_XAI_SCAN:-not run}"
  printf '| `cargo test -p workshop-adapters` (fake-CLI suite) | %s |\n' "${VERIFY_ADAPTER_TESTS:-not run}"
  [[ -n "${VERIFY_UNREGISTERED_CRATES:-}" ]] && printf '\nOverlay crates not in workspace members: `%s`.\n' "$VERIFY_UNREGISTERED_CRATES"
  [[ "${VERIFY_CARGO_LOCK_REFRESHED:-0}" == 1 ]] && printf '\n`Cargo.lock` was refreshed for the overlay crates and committed.\n'
  [[ -n "${VERIFY_SKIPPED_REASON:-}" ]] && printf '\nBuild and gates skipped: %s.\n' "$VERIFY_SKIPPED_REASON"
  printf '\n'

  printf '### Tree replacement\n\n'
  printf -- '- upstream-owned paths now mirror `%s` exactly; the import commit carries the upstream commit as second parent\n' "$short_new"
  printf -- '- %s overlay file(s) restored from `%s` (patterns: %s)\n' "${OVERLAY_FILE_COUNT:-0}" "${BASE_BRANCH:-$(short_sha "${BASE_SHA:-}")}" "$(paste -sd' ' "$R/overlay-patterns.txt" 2>/dev/null | sed 's/[^ ]*/`&`/g')"
  printf -- '- overlay collisions: %s\n' "${OVERLAY_COLLISION_COUNT:-0}"
  if [[ -s "$R/overlay-collisions.txt" ]]; then sed 's/^/  - `/; s/$/`/' "$R/overlay-collisions.txt"; fi
  printf -- '- files deleted upstream (routine): %s\n' "${UPSTREAM_DELETED_COUNT:-0}"
  printf -- '- files created by the patch series, re-created by the replay: %s\n' "${PATCH_RECREATED_COUNT:-0}"
  printf -- '- stale files dropped (neither upstream, overlay, nor patch-created): %s\n' "${DROPPED_FILE_COUNT:-0}"
  if [[ -s "$R/dropped-files.txt" ]]; then
    printf '\n<details><summary>Dropped files (in the base branch, not upstream, not overlay)</summary>\n\n```\n'
    head -n 100 "$R/dropped-files.txt"; printf '```\n</details>\n'
  fi
  printf '\n'

  n_changed=$(n_lines "$R/changed-files.txt")
  if (( n_changed )); then
    printf '<details><summary>Upstream files changed since the lock (%s)</summary>\n\n```\n' "$n_changed"
    head -n 400 "$R/changed-files.txt"
    (( n_changed > 400 )) && printf '... %s more (full list in the sync-report artifact)\n' "$((n_changed - 400))"
    printf '```\n</details>\n\n'
  fi

  printf '### How to fix a failed or deferred patch\n\n```sh\ngit fetch origin %s && git checkout %s\nscripts/sync/refresh-patch.sh <patch>            # 3-way apply, leaves conflict markers\n# resolve the markers, then\nscripts/sync/refresh-patch.sh --finish <patch>   # regenerates the patch file and commits\ngit push origin %s\n```\n\nThen prove a clean replay of `main` + the refreshed patches (see docs/upstream-sync.md, "Fixing a failed patch").\n\n' "${SYNC_BRANCH:-<branch>}" "${SYNC_BRANCH:-<branch>}" "${SYNC_BRANCH:-<branch>}"
  printf 'Policy: docs/upstream-sync.md. Generated by `scripts/sync/run.sh`%s.\n' "${run_url:+ in [workflow run]($run_url)}"
} > "$R/pr-body.md"

cat "$R/pr-body.md"
log "verdict: $verdict; title: $title; labels: ${labels[*]}"
