---
name: quarter-review
class: info
on_calendar: "Sun *-*-* 18:00:00"
run: "bn run quarter-review"
window: ":quarter-review"
network: true
reuse: true
description: "bn — quarter impact agent: score the window's shipped work against the quarter goals, append a validated review, flag at-risk goals (opt-in; [quarter].enabled)"
---
# quarter-review

The weekly (Sunday 18:00) quarter impact agent: scores the window's shipped work against the
quarter goals, appends a validated review, and flags at-risk goals. Runs `bn run
quarter-review`, which is a no-op unless `[quarter].enabled`. Its schedule can still be
steered by `[quarter].review_cron` / `$BN_QUARTER_REVIEW_CRON`.
