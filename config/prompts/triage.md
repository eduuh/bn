---
name: triage
description: Triage the bn next queue — auto-execute the safe mechanical actions, leave judgment calls
when: You want the queue swept — the obvious chores done for you, the real decisions surfaced.
keywords: [triage, sweep, queue, next, auto, mechanical, clear, chores, inbox, process, sort]
arguments:
  - { name: dry_run, description: "If set (true/yes/1), classify and report only — execute nothing", required: false }
---
You are triaging the user's `bn next` action queue: separate the safe, mechanical actions
you can do right now from the judgment calls that need the user, do the safe ones, and report.

Dry-run: if `{{dry_run}}` is truthy (`true`/`yes`/`1`), this is **plan-only** — classify and
report every item with the action you *would* take, but execute nothing and leave the queue
untouched. Otherwise execute the auto-class items as below. When in doubt, prefer the queue:
it is always safe to leave an item for the user, never safe to act on a misread one.

1. **Load the queue.** Run `bn next --json` in the shell. Each action is
   `{kind, title, detail, link?, priority, age, attn_id?}`, ranked most-pressing first. `kind` is one of
   `wip`, `cron`, `attn`, `review`, `pr`, `goal`, `blocked`, `meeting`, `email`, `mention`.
   Read it; don't re-fetch anything over the network — the queue is already the distilled,
   offline signal.

2. **Classify each item — auto vs. judgment.** Go by `kind`:

   | kind | class | why |
   |------|-------|-----|
   | `pr` (your stale in-flight PR) | **auto** — *mechanical sub-actions only* | rebase onto the base branch, run the formatter/linter, re-push; nudge a reviewer with a one-line comment. All reversible, no content decisions. |
   | `review` (a PR awaiting *your* review) | **judgment** | reviewing is the judgment; you can only fetch context, never approve. |
   | `cron` (a failing scheduled job) | **judgment** | a failure is a symptom — diagnosing it is the work. (Re-running a known-flaky job once is the only safe auto-action, and only if the user has said so.) |
   | `attn` | **split by content** — see step 3 | a flagged item may be a chore ("rebase #N") or a decision. |
   | `goal` (today's open daily goal) | **judgment** | the user's plan; never silently complete it. |
   | `blocked` (a note with open blockers) | **judgment** | unblocking needs the missing thing, which is by definition not yours to supply. |
   | `wip` (the focus guard) | **judgment** | a forward-looking nudge to *land* work, not a task to execute. |
   | `meeting` / `email` / `mention` | **judgment** | showing up, replying, and responding to being named are the user's; you can only surface them. |
   | any other kind | **judgment** | an unrecognized kind is by definition not safely mechanical. |

3. **Refine the `attn` items by reading the title.** An attn `title`/`detail` that is a pure
   mechanical chore — "rebase eduuh/bn#N", "reformat X", "re-run Y", "nudge reviewer on #N" —
   is **auto**. Anything that asks a question, names a tradeoff, or says "decide/choose/review/
   investigate" is **judgment**. If the wording is ambiguous, classify it **judgment**.

4. **Execute the auto-class items** (skip this entire step in dry-run). For each, via the harness:
   - **Rebase a stale PR**: check out its branch, `git fetch`, rebase onto the base branch. If it
     conflicts, **stop** — a conflict is a judgment call; reclassify it to the report and move on.
   - **Reformat / lint**: run the repo's formatter and linter (e.g. `cargo fmt`, the project's
     lint task). Commit only formatting/lint fixes, nothing else.
   - **Nudge a reviewer**: post one short, factual comment on the PR (`gh pr comment`) — a
     status ping, no new asks. One nudge per PR per run.
   - **Re-push** the branch when a rebase or fmt changed it.
   After each, note the outcome (succeeded / skipped-because-conflict / no-op). Never merge,
   approve, close, force-push over others' work, or touch `main` — those are out of scope here,
   full stop. If an `attn` item carries an `attn_id`, `bn attn ack <attn_id>` it **only** once
   its mechanical action genuinely completed; never ack a judgment item. (Other kinds carry no
   `attn_id` — they clear themselves from the queue when their underlying condition resolves.)

5. **Report.** Lead with a one-line tally: *N items — X auto-executed, Y left for you*
   (in dry-run: *would auto-execute X, leave Y*). Then two short groups:
   - **Done** (or **Would do** in dry-run): one line each — what you did/would do and the result.
   - **Needs you**: one line each — the item and the single judgment it's waiting on, heaviest
     first. These stay in the queue untouched.

Be conservative and honest: only auto-execute the unambiguously mechanical, only ack what truly
completed, and when a classification is even slightly uncertain, leave it for the user.
