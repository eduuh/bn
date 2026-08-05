---
name: attention-agent
class: info
on_calendar: "*-*-* 0/2:00:00"
run: "bn run attention-agent"
window: ":agent-attention-list"
network: true
reuse: true
description: "bn — attention agent: fold recent cron findings + cron health into a ranked, user-prioritizable attention list via a headless LLM (opt-in; [attention_agent].enabled)"
---
# attention-agent

The attention agent (every 2 hours): folds recent cron findings and cron health into a
ranked, user-prioritizable attention list via a headless LLM. Runs `bn run attention-agent`
(no-op unless `[attention_agent].enabled`). Pinned to the descriptive
`planning:agent-attention-list` window, the headline surface you keep open.
