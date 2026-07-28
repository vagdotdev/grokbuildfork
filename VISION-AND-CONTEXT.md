# Grok Build Fork — Vision, Context & Product Notes

**Created:** 2026-07-16  
**Location:** `~/Documents/grokbuildfork`  
**Purpose:** Capture the full conversation context, product thesis, tier plan, comparisons, and open questions for customizing SpaceXAI/xAI's Grok Build into a personal engineering terminal dashboard.

---

## 1. Origin conversation summary

This document consolidates a multi-turn discussion covering:

1. What Grok Build / Grok CLI is and whether it supports other models (BYOK)
2. Subscriptions vs API keys
3. Grok Build vs OpenCode (coding quality, then harness quality)
4. Whether forking Grok Build has merit vs just using OpenCode
5. Privacy-focused product ideas
6. A concrete product wedge: dock + import + cost meter + session context
7. What a future engineering terminal might look like (see §8)

---

## 2. Grok Build basics (facts from discussion)

### 2.1 What it is

- **Grok Build** = xAI/SpaceXAI terminal coding agent (CLI/TUI)
- Default models: xAI Grok family (e.g. `grok-4.5`, coding-oriented variants)
- Auth: `grok login` (SuperGrok / X Premium+) or `XAI_API_KEY`
- Features called out: plan mode, parallel subagents (~8), git worktrees, diffs, MCP, skills, hooks, AGENTS.md, headless `-p`, ACP, verify/sandbox proof flows

### 2.2 Custom models / BYOK

- **Yes, BYOK exists** via `~/.grok/config.toml` `[model.*]` entries
- Backends: OpenAI chat completions, OpenAI responses, Anthropic messages
- Also: `GROK_MODELS_BASE_URL` for gateway catalogs (e.g. Vercel AI Gateway)
- Switch: `/model`, `Ctrl+M`, `grok -m`, config default
- **Important:** BYOK = **API keys / endpoints**, not “paste Claude Pro / ChatGPT Plus login”
- Consumer subscriptions (Claude Pro, ChatGPT Plus, etc.) generally **do not** plug in as API keys
- Multi-model support appears present around **launch (late May 2026)**; partner docs (e.g. Vercel ~May 30) already showed multi-model gateway usage

### 2.3 Open source status (as of discussion date)

- Repo: `xai-org/grok-build` (Apache 2.0)
- Public tree for transparency / local builds
- **External PRs not accepted** (internal development) — fork = you own maintenance forever
- Context: open-sourced amid privacy/trust backlash around unexpected repo upload / telemetry concerns

---

## 3. Grok Build vs OpenCode

### 3.1 Coding quality (model + agent, not openness)

- **Coding quality ≈ model first, harness second**
- Grok Build + Grok = **frontier / A-tier**, not undisputed #1
- OpenCode coding quality depends entirely on the model plugged in
- OpenCode + Opus / GPT-5.x class often **matches or beats** Grok on hard multi-file work
- OpenCode + weak/free models → Grok Build usually wins
- Grok tends to feel strong at: speed, terminal agent loops, implementation throughput, token efficiency
- Claude-class often preferred for: careful architecture, long-horizon debugging, instruction adherence

### 3.2 Harness / agent shell only

| Dimension | Winner |
|-----------|--------|
| Overall maturity / reliability | **OpenCode** |
| Single-agent loop | **OpenCode** (slight) |
| LSP / structural repo intelligence | **OpenCode** |
| Parallel multi-agent | **Grok Build** |
| Worktree isolation | **Grok Build** |
| Plan → approve → execute | **Grok Build** (slight / more productized) |
| Capability modes on children | **Grok Build** (yes-ish) |
| Grok-native loop | **Grok Build** |
| Model flexibility | **OpenCode** |
| Ecosystem / longevity | **OpenCode** |
| Privacy defaults / trust story | **OpenCode** (generally) |

**Honest summary:**

- OpenCode = better **general-purpose** harness
- Grok Build = better **multi-agent + worktree + plan-gated** architecture *in design intent*
- Grok Build is **better in some ways**, not overall

**Where Grok Build is genuinely better:**

1. Swarm / parallel subagents as first-class
2. Git worktree isolation so agents don’t stomp each other
3. Plan-review-approve control loop
4. Capability-limited child agents
5. Native fit for Grok models
6. Verify / sandbox-boot-and-screenshot style proof flows (when present)
7. Arena / competing proposals style (when present)
8. Claude/Codex ecosystem interop surface (skills, hooks, AGENTS.md, session resume patterns)

---

## 4. Fork vs use OpenCode (strategy)

### When forking Grok Build has merit

- Multi-agent + worktrees as **core product**, not a side feature
- Clear **vertical** (security audits, monorepo incident response, airgap enterprise, etc.)
- **Privacy/trust** as the product after vendor scandals
- **Internal** company fork with compliance requirements
- Willingness to maintain a large Rust TUI/agent runtime alone

### When to just use OpenCode

- Best default daily coding harness
- Multi-model flexibility
- Community + plugins + contribution path
- No 6–12 month product wedge
- Idea is “fix UX / add features” → contribute or plugin, don’t full-fork

### Recommended builder paths (ranked)

1. **Use OpenCode** (most people)
2. **OpenCode + custom agents/plugins** (steal Grok ideas without owning runtime)
3. **Thin orchestrator** over any CLI (multi-agent/worktree logic as a layer)
4. **Internal fork of Grok Build** (control/privacy/specialized pipelines)
5. **Public niche fork** (only with clear wedge + maintenance plan)
6. **Public “better general Grok Build”** — skip; crowded market

**Architecture tip from discussion:** treat Grok Build as **agent runtime**, own product code as **dashboard layer** (dock, meter, context, import, timeline). If upstream dies, re-host the layer on OpenCode later.

---

## 5. Privacy product notes (explored, then deprioritized as headline)

Discussion concluded: **“any model + privacy” is just OpenCode with extra steps** — not the wedge.

Still useful as design principles if pursued later:

- Deny-by-default network / egress firewall
- Live egress log (every outbound call visible)
- No bulk repo upload — only explicit tool reads leave the box
- Secret redaction before model calls
- Local model default; remote allowlisted + user-approved payloads
- Capability-scoped subagents (explore read-only, implement worktree-only, etc.)
- Plan gate as **blast-radius approval**, not just step list
- Honest claim: “we don’t exfiltrate; your chosen provider only gets what you approve”

---

## 6. Product thesis (what the user actually wants)

### 6.1 In the user’s words (paraphrased + direct)

- Customize Grok Build to personal needs
- **Session-wide context** (Hermes-type thing)
- Add more over time
- **Single terminal engineering dashboard**
- Sleek and minimal
- Based on SpaceXAI’s coding agent
- Front-end animations / terminal visuals as a possible differentiator (tasteful)
- Not competing as “OpenCode but privacy”

### 6.2 Product one-liner (from discussion)

> **A minimal terminal engineering dashboard that docks the AI stack you already pay for, keeps a session-wide brain, and never blindsides you on cost or state.**

**Mental model:** Dock + meter + memory + agent  
Not “another coding agent.”

### 6.3 Elevator pitch refinement

> **Dock → Import → Meter → Session brain → Agent**  
> on a sleek Grok-descended TUI.

OpenCode is “configure any model.”  
Claude Code is “best Claude loop.”  
This product is: **the engineering dock for people who already pay for three tools and hate surprise bills and lost context.**

---

## 7. Feature tiers (user proposal + feedback)

### Tier 1 — the wedge (this IS the product)

#### User’s original list

1. **Sub detection.** On first run, scan machine — Claude Code creds, Codex/ChatGPT login, Copilot, API keys in env — show everything already paid for as ready-to-use models. Zero config. Nobody does this cleanly.
2. **`docking import claude-code` / `import codex`.** One command pulls AGENTS.md/CLAUDE.md, MCP servers, settings, slash commands. Neutral importer from everyone (Codex built Claude import for switching costs; nobody built the neutral multi-source version).
3. **Live cost meter in the footer.** Tokens, spend, context % — always visible before billed, not after. Emotional counter to surprise limits/bills.

Together = **“the dock: plug in everything you already pay for, see what you spend, never get rugged.”**

#### Feedback / adjustments

| # | Feature | Verdict |
|---|---------|---------|
| 1 | Sub detection | Best emotional hook; **riskiest** ToS/legal/brittle |
| 2 | `dock import` | **Best durable wedge** — real product identity |
| 3 | Live cost meter | **Must ship v1** — pure trust UX |

**Safer v1 for sub detection:**

- Detect **presence**, not steal sessions (“Claude Code installed ✓”, env keys ✓, config dirs exist ✓)
- Guided dock with explicit consent
- Optional deep import clearly disclosed (what files read, what never sent)
- Avoid silent hijack of OAuth/session jars (ToS + malware-adjacent feel + break on every vendor update)

**Suggested Tier 1 ship order:**

1. Cost meter (easy, instant trust)
2. Import / dock (identity)
3. Presence scan as guided dock — not credential theft

### Tier 2 — the demo moment (pick ONE)

4. **Rewind** — one keystroke reverts code + conversation to any checkpoint; scrubbable timeline. Loud feature request across Codex/OpenCode trackers. Grok Build may already have checkpoints plumbing (`xai-grok-workspace` / related crates) → UI on existing plumbing.
5. **Keep `--verify`** — sandbox-boot-and-screenshot proof; most-liked Grok Build feature; already in code; brand it, don’t strip it.

**Feedback:** Prefer **verify first** (exists, shareable demo), then **rewind** (retention).

### Tier 3 — later, if it lives

6. Memory panel (visible/editable, not black box)  
7. Sessions sidebar  
8. TUI theming / plugin API  

### Session-wide context (“Hermes-type”) — elevated importance

Discussion recommended treating this as **Tier 1.5 / spine**, not late fluff.

**What it should mean:**

Not black-box memory. A **visible session brain**:

```
SESSION CONTEXT
  goal:      ship auth refactor
  decisions: JWT over sessions; no new deps
  constraints: don’t touch billing
  open loops: migration script failing on stage
  artifacts:  plan.md, diff-a3f2, verify shot #2
```

**Mechanics:**

1. Session ledger (append-only): decisions, constraints, failures, file touch list
2. Pinned context — user-pinned facts always injected
3. Working set — last N files/tools, summarized not raw-dumped
4. Compaction you can inspect — “summarized turns 1–40 → this card” with expand/edit
5. Scope layers — session / project / global (Hermes-ish hierarchy)

**UI:** hotkey opens Context drawer (half pane), editable.

### Recommended full ship order (consolidated)

1. Shell skin — minimal dashboard layout on Grok Build  
2. Cost meter + session spend cap  
3. Session context ledger (Hermes-lite, visible)  
4. `dock import` for Claude Code + Codex + AGENTS/MCP  
5. Presence scan (“what’s installed / what’s in env”)  
6. `--verify` branded  
7. Rewind / timeline  
8. Theming, plugins, global memory store  

### Suggested TUI skeleton

```
┌─ DOCK ──────────────┬─ SESSION ─────────────────────────┐
│ Claude API  ●       │ goal / constraints / open loops   │
│ Codex       ●       │ (editable context)                │
│ Copilot     ○       │                                   │
│ Local       ●       │                                   │
├─────────────────────┴───────────────────────────────────┤
│                                                         │
│              main agent transcript / plan / diffs       │
│                                                         │
├─ PULSE ─────────────────────────────────────────────────┤
│ main*  3 dirty  tests: fail  verify: —  agents: 2 live  │
├─ FOOTER ────────────────────────────────────────────────┤
│ ctx 61%  │  turn $0.04  │  session $1.12 / $5.00  │ gpt │
└─────────────────────────────────────────────────────────┘
```

Hotkeys: context drawer, dock, rewind timeline, verify, command palette.

### Terminal visuals / animations

**Do:** state-encoding motion only

- soft pulse when agent working
- context bar ease-fill
- checkpoint tick
- cost meter color shift near limit
- subtle worktree graph
- verify success confirm (short, not fireworks)

**Don’t:** particle systems, matrix rain, delayed tokens, breaking ssh/tmux/a11y

**Rule:** motion encodes state (working / danger / checkpoint / verify). Decorative → cut.  
Aesthetic: mission control — negative space, typography, one accent color.

### Other high-fit ideas (from discussion)

| Idea | Why |
|------|-----|
| Dock panel | Providers, status, rate limits, live model |
| Spend guardrails | Hard stop / confirm at $X; per-provider budgets |
| Checkpoint timeline | Foundation for rewind + session history |
| Verify lane | Build → run → screenshot/log proof |
| Worktree map | Visual of agent branches (Grok strength) |
| Import diff | “Brought in 4 MCP, 2 skills, 1 CLAUDE.md” |
| Project pulse | git status, dirty files, last test, branch |
| Command palette | Productized minimal UX |

### Risks

1. Sub/session reuse — legal + brittle; design around consent and API keys first  
2. Import matrix — vendor configs change; start with 2 targets, not 8  
3. Cost accuracy — rates change; label estimates + rates table version/date  
4. Fork maintenance — isolate dock/context/meter as a layer  
5. Scope creep — animations/plugins eating the wedge  

---

## 8. Future engineering terminal (to be expanded in session answers)

Placeholder for ongoing thinking: what an engineering terminal from the future looks like structurally — surfaces, control plane, proof loops, multi-agent isolation, cost/context truth, etc.

*(See companion discussion after this file was written; update this section as the design hardens.)*

---

## 9. Decisions / non-goals (current)

### Goals

- Personal/custom Grok Build–based engineering dashboard  
- Session-wide visible context  
- Dock + import + cost meter as identity  
- Sleek minimal TUI; optional tasteful state animation  
- Grow features iteratively  

### Non-goals (for now)

- Winning “best general open coding agent” vs OpenCode  
- “Any model + privacy” as primary marketing  
- Silent credential theft from other CLIs  
- Full IDE replacement  
- Huge plugin marketplace on day one  

---

## 10. Open questions

- Exact crate map for checkpoints / verify / worktrees in this local tree  
- Which import targets first (Claude Code + Codex only?)  
- Rates table source of truth for cost meter  
- Local-only vs remote models as default for this personal build  
- Name/brand of the fork product  
- How much to diverge UI from upstream Grok Build vs theme/layer only  

---

## 11. Local repo note

This folder (`~/Documents/grokbuildfork`) already contains a Grok Build–related tree (Cargo workspace, crates, etc.). This markdown is product/vision context layered on top of that codebase — not a replacement for README/CONTRIBUTING.

---

## 12. Source threads (topics covered)

1. Hello / capability check  
2. Multi-model / BYOK / since when  
3. BYOK vs existing subscriptions  
4. Why use Grok Build vs OpenCode (features, then coding, then harness)  
5. Fork merit vs OpenCode  
6. Privacy fork ideas  
7. User product list (sub detection, import, cost meter, rewind, verify, tiers)  
8. Session context + dashboard + animations feedback  
9. Save context + future terminal design  

---

*End of captured context. Iterate this file as the product hardens.*
