# Shared helpers for scripts/sync/*. Source this file; do not execute it.
# shellcheck disable=SC2034  # constants are consumed by the sourcing scripts
#
# All scripts write their state into $SYNC_REPORT_DIR (default:
# $RUNNER_TEMP or $TMPDIR or /tmp, /workshop-sync-report — outside the tree)
# as a key=value env file plus a few text/TSV files. pr-body.sh renders that
# directory into the PR description.

SYNC_DEFAULT_UPSTREAM_URL="https://github.com/xai-org/grok-build"
SYNC_DEFAULT_UPSTREAM_REF="main"
SYNC_REF_NEW="refs/sync/upstream-new"
SYNC_REF_LOCK="refs/sync/upstream-lock"
SYNC_VERSION_FILE="crates/codegen/xai-grok-pager-bin/Cargo.toml"
SYNC_KNOWN_TAGS="gate:no-xai gate:no-theft product branding"
SYNC_LOCK_FILE="${SYNC_LOCK_FILE:-upstream-lock.toml}"
SYNC_PATCHES_DIR="${SYNC_PATCHES_DIR:-patches}"

log()  { printf '[sync] %s\n' "$*" >&2; }
warn() { printf '[sync] WARN: %s\n' "$*" >&2; }
die()  { printf '[sync] ERROR: %s\n' "$*" >&2; exit 1; }

repo_root() { git rev-parse --show-toplevel; }

sync_dir() { cd "$(dirname "${BASH_SOURCE[0]}")" && pwd; }

short_sha() { printf '%s' "${1:0:12}"; }

# ---------------------------------------------------------------------------
# Report directory: sync.env (key=value, shell-quoted) + free-form files.
# ---------------------------------------------------------------------------
init_report_dir() {
  SYNC_REPORT_DIR="${SYNC_REPORT_DIR:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/workshop-sync-report}"
  mkdir -p "$SYNC_REPORT_DIR"
  SYNC_REPORT_DIR="$(cd "$SYNC_REPORT_DIR" && pwd)"
  export SYNC_REPORT_DIR
}

report_set() { # report_set KEY VALUE
  local f="$SYNC_REPORT_DIR/sync.env"
  touch "$f"
  grep -v "^$1=" "$f" > "$f.tmp" || true
  printf '%s=%q\n' "$1" "$2" >> "$f.tmp"
  mv "$f.tmp" "$f"
}

report_load() {
  # shellcheck disable=SC1091
  [[ -f "$SYNC_REPORT_DIR/sync.env" ]] && source "$SYNC_REPORT_DIR/sync.env"
  return 0
}

# ---------------------------------------------------------------------------
# upstream-lock.toml (flat TOML, string values only).
# ---------------------------------------------------------------------------
lock_get() { # lock_get FILE KEY
  [[ -f "$1" ]] || return 0
  sed -n "s/^$2[[:space:]]*=[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$1" | head -n1
}

# ---------------------------------------------------------------------------
# Path ownership. Everything in the tree is upstream-owned unless it matches an
# overlay pattern. Patterns come from scripts/overlay-paths.txt when present
# (one glob or path prefix per line, '#' comments), else the built-in list that
# mirrors docs/workshop-production-plan.md section 2.
# ---------------------------------------------------------------------------
load_overlay_patterns() {
  local f
  f="$(repo_root)/scripts/overlay-paths.txt"
  if [[ -f "$f" ]]; then
    sed -e 's/#.*//' -e 's/[[:space:]]*$//' -e 's:/*$::' -e '/^$/d' "$f"
  else
    printf '%s\n' 'crates/workshop-*' patches scripts .github docs upstream-lock.toml
  fi
}

# path_matches_any PATH PATTERN... ; a pattern matches the path itself or any
# file below it. Bash globs: '*' also matches '/'.
path_matches_any() {
  local p="$1" pat
  shift
  for pat in "$@"; do
    # shellcheck disable=SC2053
    if [[ "$p" == $pat || "$p" == $pat/* ]]; then
      return 0
    fi
  done
  return 1
}

# filter_paths PATTERN... reads paths on stdin, prints those matching.
filter_paths() { # filter_paths PATTERN...
  local p
  while IFS= read -r p; do
    if path_matches_any "$p" "$@"; then printf '%s\n' "$p"; fi
  done
  return 0
}

# ---------------------------------------------------------------------------
# patches/series parser.
#
# Canonical (quilt-compatible) line:   <patch> [-pN]  # <tag> [<tag>...] [note]
# Tags are gate:no-xai, gate:no-theft, product, branding. Bare tags after the
# file name are accepted too. Output: NAME<TAB>STRIP<TAB>TAGS<TAB>NOTE<TAB>DISABLED
# A commented-out line that still carries a gate tag is emitted with DISABLED=1
# so the replay can flag it (conflict policy: gate patches are never commented
# out of the series).
# ---------------------------------------------------------------------------
parse_series() { # parse_series FILE
  local raw line name strip tags note disabled tok in_comment
  set -f
  while IFS= read -r raw || [[ -n "$raw" ]]; do
    line="${raw%$'\r'}"
    [[ -z "${line//[[:space:]]/}" ]] && continue
    disabled=0
    if [[ "$line" =~ ^[[:space:]]*# ]]; then
      line="$(printf '%s' "$line" | sed -e 's/^[[:space:]]*#*[[:space:]]*//')"
      if [[ "$line" == *gate:* && "$line" =~ ^[A-Za-z0-9_./-]+\.(patch|diff)([[:space:]]|$) ]]; then
        disabled=1
      else
        continue
      fi
    fi
    name="" strip=1 tags="" note="" in_comment=0
    for tok in $line; do
      if (( in_comment )); then
        if [[ " $SYNC_KNOWN_TAGS " == *" $tok "* ]]; then tags+="${tags:+ }$tok"; else note+="${note:+ }$tok"; fi
        continue
      fi
      case "$tok" in
        \#*)
          in_comment=1
          tok="${tok#\#}"
          [[ -z "$tok" ]] && continue
          if [[ " $SYNC_KNOWN_TAGS " == *" $tok "* ]]; then tags+="${tags:+ }$tok"; else note+="${note:+ }$tok"; fi
          ;;
        -p[0-9]*) strip="${tok#-p}" ;;
        gate:no-xai|gate:no-theft|product|branding) tags+="${tags:+ }$tok" ;;
        *) if [[ -z "$name" ]]; then name="$tok"; else note+="${note:+ }$tok"; fi ;;
      esac
    done
    [[ -n "$name" ]] && printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$strip" "$tags" "$note" "$disabled"
  done < "$1"
  set +f
}

is_gate_tags() { [[ "$1" == *gate:* ]]; }

# ---------------------------------------------------------------------------
# Git helpers.
# ---------------------------------------------------------------------------
require_clean_tree() {
  if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    die "working tree has uncommitted changes; commit or stash them first"
  fi
}

ensure_git_identity() {
  if [[ -z "$(git config user.name || true)" ]]; then
    export GIT_AUTHOR_NAME="${GIT_AUTHOR_NAME:-workshop-sync[bot]}"
    export GIT_COMMITTER_NAME="${GIT_COMMITTER_NAME:-workshop-sync[bot]}"
  fi
  if [[ -z "$(git config user.email || true)" ]]; then
    export GIT_AUTHOR_EMAIL="${GIT_AUTHOR_EMAIL:-workshop-sync@users.noreply.github.com}"
    export GIT_COMMITTER_EMAIL="${GIT_COMMITTER_EMAIL:-workshop-sync@users.noreply.github.com}"
  fi
}

# Files a patch touches (post-image paths; renames report the new name).
patch_files() { # patch_files STRIP FILE
  git apply --numstat -p"$1" "$2" 2>/dev/null | cut -f3- \
    | sed -E -e 's/\{[^}]* => ([^}]*)\}/\1/g' -e 's/^[^{]* => //' | sort -u
}

# Regenerate the diff body of patch FILE from the staged changes, keeping its
# header (format-patch mail header or free-form comment). A format-patch
# `---` separator gets a fresh diffstat. The rewritten file is staged.
refresh_patch_file() { # refresh_patch_file FILE
  local file="$1" header
  header="$(mktemp)"
  awk '/^diff --git |^--- |^Index: /{exit} {print}' "$file" > "$header"
  if grep -qx -- '---' "$header"; then
    sed -i '/^---$/,$d' "$header"
    { printf -- '---\n'; git diff --cached --stat=120 -- . ":(exclude)$file"; printf '\n'; } >> "$header"
  fi
  { cat "$header"; git diff --cached --full-index --binary -- . ":(exclude)$file"; } > "$header.patch"
  mv "$header.patch" "$file"
  rm -f "$header"
  git add -- "$file"
}

# Print the series line fields for patch NAME: STRIP<TAB>TAGS<TAB>NOTE.
series_entry() { # series_entry SERIES-FILE NAME
  parse_series "$1" | awk -F'\t' -v n="$2" '$1 == n { print $2 "\t" $3 "\t" $4; exit }'
}
