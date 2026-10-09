---
name: ship-solo
description: "Run investigate, implement and review in one session, asking only on real ambiguity. Use to ship a whole task end to end without stage handoffs."
when: A task should be delivered end to end in one session instead of the multi-session plan/consensus pipeline — one driver scopes, builds, and reviews through bounded subagents and asks a human the moment something is ambiguous. The default `bn dispatch` ship lane.
keywords: [ship-solo, solo, one session, single session, subagent, investigate implement review, no plan gate, ask when unclear, end to end, dispatch]
arguments:
  - { name: task, description: The task/goal to deliver. Omit to use the note's Goal., required: false }
  - { name: url, description: Source issue/PR url for grounding. Omit if none., required: false }
---
You are the **driver** of a single-session ship lane. Deliver the task end to end in this session
with one main owner. Use Copilot fleet / Claude subagents selectively for substantial scoping
research, self-contained implementation when delegation genuinely helps, and fresh review when
independence adds value. Investigate, implement, and review are workflow phases, not mandatory
worker slots. Start with the smallest useful team and expand only for independent work; there
is no fixed agent-count cap. Workers do not delegate further unless you explicitly authorize
an independent split.

There is no separate planning agent, no plan artifact to open in an editor, and no consensus
gate: your scoping is the plan, and a human resolves what scoping can't.

**Prime directive — never assume.** The moment something is genuinely ambiguous — unclear intent,
two valid interpretations, a fact you can't cheaply verify, or an irreversible/destructive choice —
STOP and ask with your harness's ask-the-user tool (`ask_user` in Copilot, `AskUserQuestion` in
Claude Code; "`ask_user`" below means whichever you have), then wait for the answer. A wrong guess costs more than a question.
Reversible, low-stakes calls you make yourself and record.

Keep the branch note as your **evidence ledger** as you go (`bn_add research` / `decision` /
`blocker`, `bn_pr`), not in one pass at the end — it is what outlives each subagent and this window.

1. **Frame the task.** Call `bn_cat` for context. Take the task ({{task}}) if given, else the note's
   Goal; ground on the source ({{url}}) if provided (`gh issue view` / PR body). State it in one
   sharp line and set it with `bn_goal` if the note has none. If the goal itself is unclear or
   underspecified, that is your first `ask_user` — don't proceed on a guess.

2. **Investigate — bounded evidence gathering.** Delegate substantial scoping research through
   Copilot fleet / Claude subagents with read-only constraints. Keep short known-file lookups
   and one continuous trace local. Answer exactly the scoping questions: where the change
   lives (`path:symbol`), how the affected code behaves, what the acceptance criteria are,
   and what could break — and how to REPRODUCE the problem (or demonstrate the missing behaviour):
   a failing test, a command with its wrong output, or a behaviour in the running app. Touch no
   source during investigation. Any worker returns evidence-backed findings, reproduction
   included, without the bulky search trail. Record them with `bn_add research` / `decision`.
   Follow the evidence past the first plausible answer — being right beats being fast.

3. **Decide, or ask.** From the findings, settle the approach and the **acceptance criteria** — the
   observable, checkable outcomes this repo can verify (its test gate green, a behavior a command
   proves). The reproduction passing is always the first criterion. If the issue could not be
   reproduced, that is the one outcome that ends without a PR: record the attempts and finish
   report-only. If the findings leave a real fork, a risk, or an unknown only a person can resolve,
   `ask_user` now and wait. Otherwise record the chosen approach with `bn_add decision` (name the
   alternative you rejected) and continue. Do not write a plan artifact or open an editor on one.

4. **Implement — one implementation owner.** Build the scoped change locally, or delegate a
   self-contained implementation slice when its size or isolation genuinely helps. Give any
   worker the findings, acceptance criteria, allowed write scope, and stopping condition.
   Keep integration and shared writes with the main owner. Changes stay surgical, matching
   surrounding style, nothing speculative or adjacent: every changed line traces to the task
   or a criterion. Verify as you go against `bn verify` (build/test/lint). If the scope proves
   wrong, stop and re-investigate or `ask_user`; no worker silently improvises a new task.

5. **Review — independent when useful.** For a substantial or risky change, use a fresh Copilot
   fleet / Claude subagent with the diff, acceptance criteria, reproduction, and permitted checks,
   not the implementer's reasoning. Keep a small bounded change local unless independent review
   was explicitly required. A worker already assigned as the fresh reviewer performs the review
   itself rather than launching another layer. Check each criterion against the actual diff
   with evidence (`path:line`, a command, a test), confirm `bn verify` and the reproduction pass
   on the branch head, and try to break the seams the tests miss. Return a verdict: pass, or
   concrete reproducible findings.

6. **Loop or ship.** On findings, return same-task fixes to the implementation owner or retained
   worker, then re-review the changed areas and affected checks. Do not restart implementation
   in a fresh worker merely to discard useful context. Bound retries by judgment; stop and
   `ask_user` when repeated attempts produce no new evidence or require a human decision.
   On pass, with `bn verify` green and every criterion met: commit only the files you changed (never a tree-wide
   `git add -A`), push, open the PR (`gh pr create`), and record it with `bn_pr <url>`. Then run
   `bn ingest` to fold the outcome back.

7. **Report.** One line: *scoped · repro PASS · N criteria met · verify PASS · reviewed (K rounds) · PR <url>*,
   then the single next action if anything remains. Never mark the goal done on vibes.
