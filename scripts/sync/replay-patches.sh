#!/usr/bin/env bash
# Replay patches/series onto the imported upstream tree, one commit per patch.
#
#   scripts/sync/replay-patches.sh [--patches DIR] [--series FILE] [--no-refresh]
#                                  [--report-dir DIR]
#
# Per series entry, in order (deterministic; no fuzz):
#   1. `git apply --check` clean          -> apply, status `applied`
#   2. else `git apply --3way` succeeds   -> status `applied-3way`; the patch
#      file is refreshed (header kept, diff body regenerated against the new
#      upstream) and committed together with the change
#   3. else `git apply --reject` harvests *.rej into rejects/<patch>/, the tree
#      is reset, status `failed`. gate:* (and untagged) patches make the run
#      red; product/branding patches become `deferred` and the replay goes on.
# A patch whose 3-way result is empty is `noop` (upstream already has it).
# Series lint (red): missing patch file, commented-out gate patch.
#
# Output in $SYNC_REPORT_DIR: patches.tsv, patch-detail/<patch>.md, rejects/,
# and PATCH_* / REPLAY_RED in sync.env.
# shellcheck disable=SC2016  # backticks in the detail markdown are literal
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

REFRESH=1 SERIES=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --patches) SYNC_PATCHES_DIR="$2"; shift 2 ;;
    --series) SERIES="$2"; shift 2 ;;
    --no-refresh) REFRESH=0; shift ;;
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

cd "$(repo_root)"
init_report_dir
report_load
ensure_git_identity
require_clean_tree
[[ -n "$SERIES" ]] || SERIES="$SYNC_PATCHES_DIR/series"

tsv="$SYNC_REPORT_DIR/patches.tsv"
detail_dir="$SYNC_REPORT_DIR/patch-detail"
rm -rf "$detail_dir" "$SYNC_REPORT_DIR/rejects"
mkdir -p "$detail_dir"
: > "$tsv"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

upstream_label="grok-build $(short_sha "${UPSTREAM_SHA:-$(git rev-parse HEAD)}")${UPSTREAM_VERSION:+ ($UPSTREAM_VERSION)}"
have_lock_ref=0
git show-ref --quiet --verify "$SYNC_REF_LOCK" && git show-ref --quiet --verify "$SYNC_REF_NEW" && have_lock_ref=1

declare -i total=0 n_applied=0 n_3way=0 n_noop=0 n_failed_gate=0 n_deferred=0 n_lint=0

record() { # NAME TAGS STATUS MODE NOTE FILES CHANGED_UPSTREAM
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" "$6" "$7" >> "$tsv"
}

# Restore HEAD and remove untracked leftovers (new files, *.rej) among PATHS.
reset_tree() { # FILE-LIST
  git reset -q --hard HEAD
  local f
  while IFS= read -r f; do
    [[ -n "$f" ]] || continue
    if [[ -e "$f" ]] && ! git ls-files --error-unmatch -- "$f" >/dev/null 2>&1; then
      rm -rf -- "$f"
    fi
    rm -f -- "$f.rej" "$f.orig"
  done < "$1"
}

# Write patch-detail/<name>.md: touched files, upstream commits that changed
# them since the lock (both sides of the conflict), reject hunks, apply errors.
write_detail() { # NAME STATUS FILE-LIST CHANGED-LIST REJECT-DIR ERR-FILE
  local name="$1" status="$2" files="$3" changed="$4" rejdir="$5" err="$6" out="$detail_dir/$1.md" f
  {
    printf '#### `%s` — %s\n\n' "$name" "$status"
    if [[ -s "$changed" ]]; then
      printf 'Files this patch touches that upstream changed since the lock:\n\n'
      while IFS= read -r f; do
        printf -- '- `%s`\n' "$f"
        if (( have_lock_ref )); then
          git log --format='  - upstream %h (%cs) %s' "$SYNC_REF_LOCK..$SYNC_REF_NEW" -- "$f"
        fi
      done < "$changed"
      printf '\n'
    elif [[ -s "$files" ]]; then
      printf 'None of the files this patch touches changed upstream since the lock; the failure comes from an earlier patch in the series or a stale patch.\n\n'
    fi
    if [[ -s "$files" ]]; then
      printf '<details><summary>Files touched by the patch</summary>\n\n```\n'; cat "$files"; printf '```\n</details>\n\n'
    fi
    if [[ -d "$rejdir" ]] && find "$rejdir" -type f -name '*.rej' | grep -q .; then
      printf '<details><summary>Rejected hunks</summary>\n\n'
      while IFS= read -r f; do
        printf '`%s`\n\n```diff\n' "${f#"$rejdir"/}"
        head -n 120 "$f"
        [[ "$(wc -l < "$f")" -gt 120 ]] && printf '... (truncated; full file in the sync-report artifact)\n'
        printf '```\n\n'
      done < <(find "$rejdir" -type f -name '*.rej' | sort)
      printf '</details>\n\n'
    fi
    if [[ -s "$err" ]]; then
      printf '<details><summary>git apply output</summary>\n\n```\n'; head -n 60 "$err"; printf '```\n</details>\n\n'
    fi
  } > "$out"
}

if [[ ! -f "$SERIES" ]]; then
  warn "no patch series at $SERIES; nothing to replay (M0 overlay not present?)"
  report_set SERIES_PRESENT 0
  report_set PATCH_TOTAL 0
  report_set REPLAY_RED 0
  exit 0
fi
report_set SERIES_PRESENT 1
report_set SERIES_FILE "$SERIES"

while IFS=$'\t' read -r name strip tags note disabled; do
  total+=1
  file="$SYNC_PATCHES_DIR/$name"
  gate=0
  if is_gate_tags "$tags"; then gate=1; fi
  if [[ -z "$tags" ]]; then
    warn "$name has no tag in series; treating it as non-deferrable"
    gate=1
    tags="(untagged)"
  fi

  if (( disabled )); then
    warn "$name: gate patch is commented out of the series (conflict policy rule 1)"
    record "$name" "$tags" disabled "commented out of series" "$note" "" ""
    n_lint+=1
    continue
  fi
  if [[ ! -f "$file" ]]; then
    warn "$name: listed in series but $file does not exist"
    record "$name" "$tags" missing "file not found" "$note" "" ""
    n_lint+=1
    continue
  fi

  patch_files "$strip" "$file" > "$tmp/files.txt"
  : > "$tmp/changed.txt"
  if [[ -s "$SYNC_REPORT_DIR/changed-files.txt" && -s "$tmp/files.txt" ]]; then
    cut -f2- "$SYNC_REPORT_DIR/changed-files.txt" | sed 's/.*\t//' | sort -u \
      | comm -12 - <(sort -u "$tmp/files.txt") > "$tmp/changed.txt"
  fi
  files_csv="$(paste -sd, "$tmp/files.txt")"
  changed_csv="$(paste -sd, "$tmp/changed.txt")"
  : > "$tmp/err.txt"

  status="" mode=""
  if git apply --check -p"$strip" "$file" 2> "$tmp/err.txt"; then
    git apply --index -p"$strip" "$file"
    status=applied mode="clean"
  elif git apply --3way -p"$strip" "$file" > "$tmp/err.txt" 2>&1; then
    status=applied-3way mode="3-way merge"
  else
    reset_tree "$tmp/files.txt"
    git apply --reject -p"$strip" "$file" > "$tmp/reject.txt" 2>&1 || true
    rejdir="$SYNC_REPORT_DIR/rejects/$name"
    while IFS= read -r f; do
      if [[ -n "$f" && -f "$f.rej" ]]; then
        mkdir -p "$rejdir/$(dirname "$f")"
        mv "$f.rej" "$rejdir/$f.rej"
      fi
    done < "$tmp/files.txt"
    reset_tree "$tmp/files.txt"
    if (( gate )); then
      status=failed mode="does not apply (3-way conflict)"
      n_failed_gate+=1
      log "RED: $name [$tags] failed to apply"
    else
      status=deferred mode="does not apply (3-way conflict); non-gate, skipped"
      n_deferred+=1
      log "deferred: $name [$tags] failed to apply"
    fi
    write_detail "$name" "$status" "$tmp/files.txt" "$tmp/changed.txt" "$rejdir" "$tmp/err.txt"
    record "$name" "$tags" "$status" "$mode" "$note" "$files_csv" "$changed_csv"
    continue
  fi

  if git diff --cached --quiet; then
    status=noop mode="upstream already contains this change"
    n_noop+=1
    reset_tree "$tmp/files.txt"
    log "noop: $name [$tags] (upstream already contains it; consider dropping the patch)"
  else
    if [[ "$status" == applied ]]; then
      n_applied+=1
    else
      n_3way+=1
      if (( REFRESH )); then
        refresh_patch_file "$file"
        mode="3-way merge; patch file refreshed"
      fi
    fi
    git commit -q -m "patches: $name [$tags]" -m "Replayed onto $upstream_label; $mode."
    log "$status: $name [$tags]"
  fi
  if [[ "$status" == applied-3way || "$status" == noop ]]; then
    write_detail "$name" "$status" "$tmp/files.txt" "$tmp/changed.txt" "/nonexistent" "$tmp/err.txt"
  fi
  record "$name" "$tags" "$status" "$mode" "$note" "$files_csv" "$changed_csv"
done < <(parse_series "$SERIES")

replay_red=0
(( n_failed_gate > 0 || n_lint > 0 )) && replay_red=1

report_set PATCH_TOTAL "$total"
report_set PATCH_APPLIED "$n_applied"
report_set PATCH_3WAY "$n_3way"
report_set PATCH_NOOP "$n_noop"
report_set PATCH_FAILED_GATE "$n_failed_gate"
report_set PATCH_DEFERRED "$n_deferred"
report_set PATCH_LINT_ERRORS "$n_lint"
report_set REPLAY_RED "$replay_red"

log "replay: $total patch(es): $n_applied clean, $n_3way 3-way, $n_noop noop, $n_deferred deferred, $n_failed_gate gate failure(s), $n_lint series error(s)"
(( replay_red )) && log "replay is RED: a gate patch failed or the series is invalid; do not merge"
exit 0
