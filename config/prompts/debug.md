---
name: debug
description: Reproduce, isolate, root-cause and fix a known failure — repro-first, verified, learning captured
when: Something is broken — a failing test, an error, wrong behavior — and you want it fixed properly, not patched over. A known failure with a fix at the end.
keywords: [debug, bug, fix, broken, failing, error, crash, regression, root cause, repro, why is this happening, not working]
arguments:
  - { name: symptom, description: The observed failure. Omit to take the note's top bug/blocker., required: false }
---
You are fixing a **known failure** — reproduce it, isolate it, name the root cause, fix that
cause, and prove it's gone. This is not open research (that's `investigate`) and not building new
work (that's `plan-work`): there is a broken thing and a passing gate at the end. The discipline
is **repro before fix, cause before patch** — a fix you can't reproduce failing you can't prove
you made.

1. **Reproduce first.** Call `bn_cat` for context; take the symptom ({{symptom}}) or the note's top bug. Before
   touching a cause, make the failure happen *on demand* — ideally a failing test or an exact
   command + expected-vs-actual. If you can't reproduce it, that's the whole task: say so and pin
   down the missing conditions before going further. Log the repro with `bn_add research`.

2. **Isolate.** Narrow from symptom to mechanism — bisect, add targeted logging, halve the search
   space. Note what you *rule out* (`bn_add research`), not just hunches; the ruled-out paths are
   half the value. Keep going until you can point at the line/condition responsible.

3. **Name the root cause before fixing.** State the actual cause in one line — the real reason, not
   the surface symptom — and record it with `bn_add decision`. If you're fixing without being able
   to say why it broke, you're guessing; go back to step 2.

4. **Fix the cause, surgically.** Change the minimum that addresses the root cause — no drive-by
   refactors, no fixing adjacent things you noticed. If the real fix is large or risky, say so and
   propose it rather than forcing it in.

5. **Prove it's gone.** Re-run the step-1 repro: it must now pass. Then run the repo's full gate —
   `bn verify` (build/test/lint gate) via the harness — so the fix didn't break anything else. Not
   green means not done. Where a test would have caught this, add it now.

6. **Capture the learning.** Record the non-obvious cause with `bn learned "…"` — the thing that
   would've saved you an hour. Close the bug todo with `bn_done`.

7. **Report.** One line: *root cause → fix → verify PASS*. Then the repro, the cause, what changed,
   and any follow-up filed. If it's a symptom of something deeper, `bn_add todo` the real fix
   rather than pretending the patch closed it.

If the flow itself misfired — a step out of order, a missing move — capture it with
`bn reflect "skill:debug — <what to change>"` (via the harness). `evolve-skills` folds those
reflections back into this file.
