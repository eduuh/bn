---
name: task-triage
on_calendar: "*-*-* 07:30:00"
run: "bn task triage"
network: false
description: "bn — task triage: rank every open task deterministically, then pick and stamp what is genuinely doable today"
---
# task-triage

Each morning, decides what is actually doable today.

The ranking is **not** the model's. `bn_core::task_agents::pre_score` scores every open task in
ordinary Rust — priority, overdue-ness, in-flight, estimate size, whether it passed the gate — so
the same store on the same day always produces the same order. The agent only reorders the top
twelve, stamps `day:`, and writes one line per decision explaining why.

That split is deliberate: a pure-LLM ranking is reinvented on every run, so the list you trusted
yesterday would mean nothing today. Here the deterministic score decides *what is in play* and the
model applies judgement *within* it — and it may not reach past the shortlist.

Triage rewrites the shared task store wholesale, so it is gated on `[staff].home`: it runs on one
machine, not on every laptop that syncs the notes repo.

`bn task triage --dry-run` prints the ranking and the seed without launching anything.
