# Workshop release notes

Install or update: `curl -fsSL https://raw.githubusercontent.com/vagdotdev/grokbuildfork/release-channel/install.sh | sh`
(or `workshop update`). Issues and source: https://github.com/vagdotdev/grokbuildfork

## 0.2.2

- One product name, everywhere: **Vagdev's Workshop**. The welcome card is an invitation to type
  (`Ask anything…` placeholder), shows *Resume session* only when there is something to resume, and
  its *Release notes* row opens these notes.
- Waiting is visible: an animated indicator on `Waiting for <model>…` and on the engine bring-up,
  elapsed seconds after 3 s, a `Ctrl+C to cancel` hint, and byte progress while the OpenCode engine
  downloads. A model that has not answered after 10 s says `Still connecting…`.
- `/model` and `/auth` are overlays over the transcript. The model list has type-to-filter, a
  *Recommended* group, a stable order while open, and hides classifiers and routers behind
  `Ctrl+A show all`. Keys never leak out of an open picker into the prompt.
- Errors are red, fast and actionable: `Enter` retries the failed prompt, `/model` switches model,
  `/doctor` checks the setup. `/release-notes` works offline.
- Help, tutorial and the bundled guides describe Workshop: `workshop --help`, `~/.workshop`,
  `WORKSHOP_*` environment variables (the old names keep working), `/docs` Getting Started tells the
  real install and first-run story.
- `/feedback` says where the note is saved and drafts a prefilled GitHub issue. The installer labels
  every step, no longer downloads the 142 MB voice model up front (first `/voice` fetches it, or
  `WORKSHOP_VOICE=1`), and ends with `cd <project> && workshop`.
- The terminal title follows the session topic while Workshop runs and is restored on exit. The `/`
  menu and the command palette show the common commands first and the power tools under *Advanced*.
- Engine trust: true safety modes for the engine, diffs and command output visible in the
  transcript, resume works, honest context meter, queued prompts sent as separate turns.

## 0.2.1

- First run lands in the composer with OpenCode's default free model active (Big Pickle); nothing to
  read or choose before typing. `/model` and `/auth` are the only doors.
- Model lists are live: the OpenCode engine list follows every `opencode serve` start, the keyless
  catalogs (Kilo, OpenRouter, NVIDIA) are fetched on `/model` and cached under `~/.workshop`.
- Engine bring-up is visible and every failure is one line with the cause and a way out.
- Workshop reads only `~/.workshop`; another app's `~/.grok`, hooks and sessions are never touched.
- The welcome hero is a `v` monogram; themes are **Night** and **Day**.

## 0.2.0

- First public release: one-line installer with SHA-256 verification, signed provenance on the
  release assets, the OpenCode engine for free models, subscription rails for Claude / Codex /
  Cursor through their official CLIs, API-key providers stored in the OS keyring, local voice
  dictation.
