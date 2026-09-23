# Workshop release notes

Install or update: `curl -fsSL https://raw.githubusercontent.com/vagdotdev/grokbuildfork/release-channel/install.sh | sh`
(or `workshop update`). Issues and source: https://github.com/vagdotdev/grokbuildfork

## 0.2.2

- One product name, everywhere: **Vagdev's Workshop**. The welcome card is an invitation to type
  (`Ask anything…` placeholder), shows *Resume session* only when there is something to resume, and
  its *Release notes* row opens these notes.
- Waiting is one calm line, `Thinking…`, whatever happens behind it: an animated mark, elapsed
  seconds after 3 s, `Ctrl+C to cancel`, and the download progress on a first run.
- `/model` and `/auth` are overlays over the transcript. `/model` lists OpenCode's free models,
  then each installed coding subscription (Claude, Codex, Cursor) with its models or a `Sign in`
  row, then the API-key providers you connected; type to filter, stable order while open,
  classifiers and routers hidden behind `Ctrl+A`. Keys never leak out of an open picker.
- The composer's border reads `<model> (<effort>) · <mode>` — `Big Pickle · always-approve`,
  `Ling 3.0 Flash Fin Free (high) · plan`. The model by name only; the effort
  level when the model offers levels and you picked one on `/model` (each level is its own row,
  and the pick is what the model actually runs at); the mode when it is not the default. If the
  free default cannot be reached, Workshop quietly answers through another free model and names
  it; only when that fails too do you see `Couldn't reach Big Pickle — Enter to retry · /model to
  switch`.
  `/release-notes` works offline.
- Help, tutorial and the bundled guides describe Workshop: `workshop --help`, `~/.workshop`,
  `WORKSHOP_*` environment variables (the old names keep working), `/docs` Getting Started tells the
  real install and first-run story.
- `/feedback` says where the note is saved and drafts a prefilled GitHub issue.
- The installer downloads one thing, `workshop` — no voice helper or speech model up front
  (`WORKSHOP_VOICE=1` installs voice right away) — reads like a product (`Verifying… done`,
  `Installing… done`), says what it did (`Installed Workshop 0.2.2`, or `Updated Workshop 0.2.0 →
  0.2.2` over an earlier install, a manual tar install included) and ends with
  `cd <project> && workshop`.
- The terminal title follows the session topic while Workshop runs and is restored on exit. The `/`
  menu and the command palette show the common commands first and the power tools under *Advanced*.
- Trust: true safety modes for the free models, diffs and command output visible in the
  transcript, resume works, honest context meter, queued prompts sent as separate turns.

## 0.2.1

- First run lands in the composer with OpenCode's default free model active (Big Pickle); nothing to
  read or choose before typing. `/model` and `/auth` are the only doors.
- Model lists are live: OpenCode's list follows every `opencode serve` start, the hosted catalogs
  are fetched on `/model` and cached under `~/.workshop`.
- Setting up the free models is visible and every failure is one line with a way out.
- Workshop reads only `~/.workshop`; another app's `~/.grok`, hooks and sessions are never touched.
- The welcome hero is a `v` monogram; themes are **Night** and **Day**.

## 0.2.0

- First public release: one-line installer with SHA-256 verification, signed provenance on the
  release assets, OpenCode's free models, subscription rails for Claude / Codex /
  Cursor through their official CLIs, API-key providers stored in the OS keyring, local voice
  dictation.
