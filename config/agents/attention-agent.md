---
name: attention-agent
on_calendar: "*-*-* 0/2:00:00"
run: "bn run attention-agent"
network: true
description: "bn — attention agent: fold recent cron findings + cron health into a ranked, user-prioritizable attention list via a headless LLM (opt-in; [attention_agent].enabled)"
---
# attention-agent

The attention agent (every 2 hours): folds recent cron findings and cron health into a
ranked, user-prioritizable attention list via a headless LLM. Runs `bn run attention-agent`
(no-op unless `[attention_agent].enabled`). Read the result with `bn attn`.
