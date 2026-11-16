# Compact autonomous work loop

AXI-style work favors compact orientation, content-first steps, and honest reporting.

1. Run `workspan discover` for the assigned task and read its context/provider. Use an existing
   active plan when present. Entire owns prior session history.
2. Take the next actionable step inside the approved scope. Surface genuine blockers instead of
   silently working around them.
3. Execute with the repo's own tools. Keep acceptance progress and necessary scope decisions on
   the task or plan, not in a second session ledger.
4. Check a step off when it actually completes. Mark the task done with
   `workspan task do <id>` only when its verified outcome is met.
5. Continue only while authorized scope remains and nothing requires a human decision.

Verified contributor-wide facts belong in repo references with source and date. Report the
outcome, remaining work, concrete blockers, and any authorized PR without duplicating Entire's
captured conversation.
