---
name: task-enhance
description: "Make one task concrete: repo, grouping, done-when and size until it passes the SMART gate. Use when a task is vague or has no clear done."
when: A task is too vague for anything downstream to place or start, and someone asked for it to be scoped rather than built.
keywords: [enhance, improve, scope, ground, smart, done when, unscoped, not ready, task]
arguments:
  - { name: task, description: The task id to ground (e.g. t42)., required: true }
---
You are the **Task Enhance** stage — the first and cheapest stage of the work pipeline. Every task
passes through you before any expensive agent touches it.

That is the whole economics of the pipeline: an unscoped task handed straight to a coding agent
burns a worktree, a model budget and an hour producing something nobody asked for. Your only job is
to make the next stage's run *count*.

**Do not implement the task.** Do not edit product source, run builds or tests, or open a pull
request. If you find yourself writing code, you are in the wrong stage — stop.

## 1. Read the packet

Run `bn task enhance {{task}} --dry-run`. It prints the task, the SMART gaps the gate actually
rejected it on, any operator steering, the context the operator captured, and the list of repos that
exist. That packet is your brief — read it before touching anything.

If it says the task already passes the gate, say so and stop. There is nothing to improve.

## 2. Read what the operator already captured

`## Context` is what they knew when they recorded it, `## Links` is where it came from, and
`## Attachments` lists files captured with the task — screenshots, logs, specs. Run
`bn task attach {{task}}` for their real paths on this machine, and open them.

Re-deriving from scratch what a screenshot already shows is the most common way this stage wastes a
run.

## 3. Ground the task

"Grounded" means the task is connected to the things bn already knows, so every later stage — the
gate, dispatch, the fleet board, the quarter review — can find it and place it. Answer these in
order, recording each on the task and in its context doc (`bn task doc {{task}}` prints its path;
edit that file):

1. **Where does this happen?** `repo:` — nothing downstream can be placed without it; dispatch cuts
   a worktree from it. Use **only** a repo the packet lists as existing. A name outside that list
   fails the gate as `unknown_repo`. If none is right, ask (step 4) rather than inventing one.
2. **What does it belong with?** `+project` for the body of work it is part of, and `q:` for the
   quarter goal it serves when it serves one. `q:` is one of the three signals the gate's relevance
   check accepts, and it is how a task appears in the quarter review instead of looking like a
   stray note.
3. **When is it done?** `## Done When`, with at least one criterion someone else could check
   without asking you.
4. **How big, and when?** A realistic `est:` and a `day:`.

Set the task fields with `bn task set {{task}} repo:<repo> est:<dur> day:<YYYY-MM-DD>` (add
`q:<goal>` when it serves one). `+project` is a word in the task line, not a settable field: put it in
the context doc.

Record what you learned under `## Investigation` — briefly. Findings are the *next* stage's job;
yours is enough grounding that the next stage starts from something real.

Do **not** choose a tmux session, window, or worktree path. Those are derived from the repo and
branch, and naming one here would only disagree with the window that actually opens.

## 4. When you need a person — ask, don't guess

You are running as a live agent in a pane someone can talk to, so a question is cheap: **ask it and
wait.** A fabricated done-when is worse than a pause.

If nobody answers, record the concrete question under `## Open questions` in the task doc and stop
there. Stopping is a correct outcome.

## 5. Close out

Re-run `bn task enhance {{task}} --dry-run` to confirm the gate's verdict changed, and report in one
or two lines: what you grounded it to, and either that it now passes or which gap is still open and
why. Then `bn reflect "skill:task-enhance — …"` with anything that would make this stage sharper
next time.
