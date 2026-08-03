---
name: today-agent
on_calendar: "Mon..Fri *-*-* 08:30:00"
run: "bn run today-agent"
network: false
description: "bn — Today's agent (#577): synthesize the cron findings store (#576) into reviewable suggested tasks (proposals + source finding + rationale), surfaced on Today; idempotent (opt-in; [today].enabled)"
---
# today-agent

Today's agent (weekday mornings 08:30, after the AM findings agents have gathered):
synthesizes the cron findings store into reviewable suggested tasks — each a proposal with
its source finding and rationale — surfaced on Today. Runs `bn run today-agent`; idempotent,
no-op unless `[today].enabled`.
