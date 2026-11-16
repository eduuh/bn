---
name: debug
description: "Reproduce and root-cause a known failure, fixing it only within the requested write scope. Use when asked to reproduce, diagnose or fix a failure, not merely explain an error message or interpret logs."
when: Reproduction, diagnosis or repair of a known failure was requested. Simple error explanations and log interpretation do not require this workflow.
keywords: [debug, bug, fix, broken, failing, error, crash, regression, root cause, repro, why is this happening, not working]
arguments:
  - { name: symptom, description: The observed failure. Omit to use the assigned task., required: false }
---
Not open research (that's `investigate`) and not new work (that's `build-from-plan`). The discipline
is **repro before fix, cause before patch**: a fix you can't reproduce failing you can't prove.
The requested scope determines whether the result is diagnosis or a verified code fix.
For diagnosis-only requests, gather evidence without editing source or tests; do not add
diagnostic logging that changes code.

1. **Reproduce first.** Read the assigned task's context and call `bn_ref` (no name) for what past
   sessions already learned about this repo — this failure may be a known gotcha, and reading a
   one-line index is cheaper than root-causing it twice. Take the symptom ({{symptom}}) or the assigned bug. Before
   touching a cause, make the failure happen *on demand* — ideally a failing test or an exact
   command + expected-vs-actual. If you can't reproduce it, that's the whole task: say so and pin
   down the missing conditions before going further. Keep the repro evidence on the task.

2. **Isolate.** Narrow from symptom to mechanism — bisect, add targeted logging, halve the search
   space. Keep evidence of what you *rule out* on the task, not just hunches; the ruled-out paths are
   half the value. Keep going until you can point at the line/condition responsible.
   When the cause depends on runtime state, invoke `nvim-debug`; use observed frames and
   values rather than guessing from source.
   Keep the main root-cause trace local. When the reproduction leaves substantial independent
   hypotheses, use Copilot fleet / Claude subagents to test those hypotheses read-only and
   return evidence that confirms or rules them out. Do not divide one call chain among workers
   or let several workers patch the same failure. If already assigned as a debugging worker,
   complete your slice without further delegation unless the parent explicitly authorizes it.

3. **Name the root cause before fixing.** State the actual cause in one line — the real reason, not
   the surface symptom — and record the actionable cause on the task. If you're fixing without being able
   to say why it broke, you're guessing; go back to step 2.

4. **Respect the requested write scope.** For a diagnosis-only request, report the repro,
   evidenced cause, and remaining uncertainty, then stop before editing. The remaining repair,
   verification, and completion steps apply only when fixing code was authorized.
   Change the minimum that addresses the root cause — no drive-by
   refactors, no fixing adjacent things you noticed. If the real fix is large or risky, say so and
   propose it rather than forcing it in.

5. **Prove it's gone.** Re-run the step-1 repro: it must now pass. Then run the repo's full gate —
   `bn verify` (build/test/lint gate) via the harness — so the fix didn't break anything else. Not
   green means not done. Where a test would have caught this, add it now.

6. **Capture the learning.** Record the non-obvious cause with `bn learned "…"` — the thing that
   would've saved you an hour. Then ask whether it is *durable repo knowledge*: will it trip up
   the next session on this repo, not just this branch? If so, fold it into the repo's reference
   notes with `bn_ref_append` — one line, under `gotchas`, ending in where it came from and
   today's date (see `/bn:repo-reference`). This is the step that stops the same cause
   being root-caused twice. Close the assigned bug with `workspan task do <id>` only when verified.

7. **Report.** One line: *root cause → fix → verify PASS*. Then the repro, the cause, what changed,
   and any follow-up filed. If it's a symptom of something deeper, capture the real fix as a task
   rather than pretending the patch closed it.
