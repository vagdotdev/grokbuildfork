# Plan Mode & Permissions

Workshop asks before doing anything risky — and can plan before it codes.

## Permissions

When Workshop wants to run a risky command or edit a file, it pauses and asks:
allow once, always allow that kind of action, or deny.

Reading is always free: file reads, searches, and safe read-only commands
(`ls`, `git status`, `grep`, …) never prompt; commands that delete, install
or reach the network do.

Trust the session? `/always-approve` (or `Ctrl+O`) skips the prompts.

## Plan mode

For bigger or more ambiguous tasks, use **plan mode**: Workshop explores the
codebase read-only, designs an approach, and presents a plan you approve
*before* any code is written.

- **`Shift+Tab`** (prompt focused) cycles the mode: Normal → Plan →
  Always-approve.
- **`/plan`** enters plan mode directly; `/plan <task>` plans that task in
  one step.

When the plan is ready: `a` approves, `c` comments on a specific line,
`s` requests changes — Workshop iterates until you're happy, then implements.

A good habit: plan mode for "how should we even do this?", normal mode for
"just do it".

## Long-running commands

A build or test run hogging the turn? **`Ctrl+B`** sends it to the
background — Workshop keeps working and you're notified when it finishes
(`Ctrl+G` shows the tasks pane).

*Go deeper: `/docs Plan Mode` or `/docs Permissions and Safety`*
