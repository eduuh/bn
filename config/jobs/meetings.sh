#!/bin/sh
# meetings.sh — surface today's meetings that actually matter. Microsoft 365 Copilot (the
# `workiq` CLI) holds the calendar and judges importance; bn renders. Two surfaces, mirroring the
# README's feed/attention split:
#
#   - the FEED gets every important meeting (`bn feed ingest meetings`). Each run *replaces* the
#     source, so finished meetings fall off on their own — no expiry logic here.
#   - the ATTENTION LIST gets the single most-important meeting (`bn attn add`), so it rides in
#     `bn next`. Severity `info`, so it never toasts; it's an anchor, not an alarm.
#   - the FEED source `meeting-prep` gets a short brief for that next meeting (agenda, your role,
#     the latest relevant thread, a doc to skim) — one extra workIQ call, only when one's upcoming.
#
# Pipeline: workiq answers (and judges which meetings are skippable) → a shared local Copilot pass
# (lib/m365-json.sh) coerces that prose into strict JSON → jq fans it out to the two surfaces.
#
# Best-effort and non-destructive: if workiq is absent/unauthenticated or yields nothing, the
# feed and attention list are left as-is rather than cleared. workiq lives only on the work
# laptop, so this is a clean no-op elsewhere — pair with `[jobs.only_on] meetings = ["<laptop>"]`
# to keep the timer off other machines too. Triggered by `bn run meetings`; scheduled at 07:00 +
# 14:50 in crons.toml, and re-fired on the first bn event of the day via `bn run meetings
# --if-stale`.
set -eu

bn="${BN:-$HOME/.config/bn/bn}"
. "$(dirname "$0")/lib/m365-json.sh"   # m365_query_json — WorkIQ + local Copilot JSON normalizer

command -v workiq >/dev/null 2>&1 || { echo "meetings: workiq not found — skipping" >&2; exit 0; }
command -v jq     >/dev/null 2>&1 || { echo "meetings: jq not found" >&2; exit 0; }

today=$(date +%F)
weekday=$(date +%A)

# 1) Ask workIQ for today's meetings, labelled by importance. It LISTS every meeting (so the feed
#    shows the whole day) and judges how much each matters — it does not drop any. The script then
#    decides placement: all → feed, the top important+upcoming one → attention.
question="List ALL my Outlook calendar meetings for today, $weekday $today — do not omit any. For
each, give the start time (HH:MM, 24-hour), the subject, the Teams join link if present, and rate
how important it is for me to attend as one word: critical, high, medium, or low. Use low for
optional, FYI/broadcast, or recurring meetings where I'm not a required attendee. One line per
meeting."

wq_timeout="${BN_MEETINGS_WORKIQ_TIMEOUT:-150}"

# 2) Coerce the prose into strict JSON. workiq already judged importance; Copilot only reformats —
#    it must not invent meetings or re-rank. Emits a JSON array, most-important first.
#    Reliable links: workIQ rarely has a click-to-join URL, but it DOES attach a per-meeting Teams
#    link as a footnote marker — `[N](https://teams.microsoft.com/l/meeting/details?eventId=…)` —
#    that opens the meeting in Teams (where you join). The notes/footnotes say which [N] is which
#    meeting; Copilot reads that mapping and sets joinUrl from it.
schema='Output ONLY a JSON array (no prose, no code fence), sorted most-important first
(critical > high > medium > low), each element:
{"id": "<stable slug from time+subject>", "start": "<HH:MM 24h, or empty>", "subject": "<text>",
 "joinUrl": "<the https://teams.microsoft.com/l/... link associated with THIS meeting, else empty>",
 "importance": "critical|high|medium|low"}.
Meetings are usually followed by footnote links like [1](https://teams.microsoft.com/l/...),
[2](...). The text (a notes section) states which footnote number belongs to which meeting — use
that to attach each meeting its OWN link as joinUrl; never reuse one link for a different meeting
or invent a URL. Use only meetings present in the text. Empty array if none.'

meetings_rc=0
# Bound generously — the footnote URLs are long (~300 chars each), and truncating them would lose
# exactly the join links we are after.
meetings=$(m365_query_json "$wq_timeout" "$question" "$schema") || meetings_rc=$?
if [ "$meetings_rc" -eq 1 ]; then
  echo "meetings: workiq returned nothing within ${wq_timeout}s — leaving feed/attention as-is"
  exit 0
fi

count=$(printf '%s' "${meetings:-}" | jq 'length' 2>/dev/null || echo 0)
if [ "${count:-0}" -eq 0 ]; then
  echo "meetings: no important meetings parsed for $today — leaving feed/attention as-is"
  exit 0
fi

# 3) FEED — replace the `meetings` source with today's meetings, time-ordered (missing times last).
printf '%s' "$meetings" \
  | jq -c 'sort_by((.start // "") | if . == "" then "99:99" else . end)[]
           | {key: .id, text: (((.start // "") | if . == "" then "??:??" else . end) + "  " + .subject),
              url: (.joinUrl // "")}' \
  | "$bn" feed ingest meetings

# 3b) GAP CACHE — pipe the same day's events into `bn gap ingest`, which stamps them into the one
#     documented cache (`<state>/meetings.json`) that `bn gap` and `bn next --fit` read (#247). The
#     script fetches, bn renders — no calendar parsing lives in bn. A run REPLACES the cache, so
#     finished meetings and prior days drop off on their own. Best-effort: never abort the sweep.
printf '%s' "$meetings" \
  | jq -c '.[] | {start: (.start // ""), title: .subject, url: (.joinUrl // "")}' \
  | "$bn" gap ingest || true

# 4) ATTENTION — clear any stale meeting items (prior days, or an earlier top that's changed),
#    then raise today's single most-important one. Info severity: shows in `bn next`, never toasts.
"$bn" attn list --json 2>/dev/null \
  | jq -r '.data.items[]? | select(.source=="meetings") | .id' 2>/dev/null \
  | while IFS= read -r id; do [ -n "$id" ] && "$bn" attn ack "$id" >/dev/null 2>&1 || true; done

now=$(date +%H:%M)
# Anchor on the most important meeting that's both still ahead AND genuinely worth interrupting for
# (critical/high) — so the attention list never nags about an optional/low item, and the afternoon
# refresh drops finished ones. Missing start → treated as upcoming (surface, not drop). The feed
# keeps showing the whole day regardless. `meetings` is already importance-sorted, so .[0] wins.
top=$(printf '%s' "$meetings" | jq -c --arg now "$now" '
  def rank: {"critical":3,"high":2,"medium":1,"low":0}[(.importance // "" | ascii_downcase)] // 0;
  [ .[] | select((.start // "99:99") >= $now and rank >= 2) ]
  | sort_by(rank) | reverse | .[0] // empty')

if [ -n "$top" ]; then
  subject=$(printf '%s' "$top" | jq -r '.subject')
  start=$(printf '%s'   "$top" | jq -r '.start // ""')
  join=$(printf '%s'    "$top" | jq -r '.joinUrl // ""')
  # `|| true`: a failed add must not abort before the meeting-prep step (5) runs.
  "$bn" attn add "$start $subject" --severity info --source meetings --key "meeting-$today" \
    ${join:+--link "$join"} >/dev/null || true
  echo "meetings: $count meeting(s) → feed; next-up → attention ($start $subject)"

  # 5) MEETING PREP — a short brief for that next meeting, into its own feed source (the feed holds
  #    multi-line content; the attention list is one anchor line). One extra workIQ call, and only
  #    when a meeting is actually coming up. Best-effort: a stall leaves the last prep as-is.
  prep_q="Prepare me for my meeting \"$subject\"${start:+ at $start} today. In a few short bullets:
its purpose/agenda, my role or what's expected of me, the most relevant recent email or Teams
thread, and any document I should skim. Include links where available."
  prep_schema='Output ONLY a JSON array (no prose, no code fence) of 2-5 short prep bullets, each:
{"id":"<stable slug>","text":"<one short line>","link":"<URL or empty string>"}.
Ground every bullet in real context from the data; never invent a fact or URL. Empty array if
there is nothing useful to say.'
  prep=$(m365_query_json "$wq_timeout" "$prep_q" "$prep_schema" || true)
  if [ -n "$prep" ] && [ "$(printf '%s' "$prep" | jq 'length')" -gt 0 ]; then
    printf '%s' "$prep" | jq -c '.[] | {key:.id, text:.text, url:(.link//"")}' \
      | "$bn" feed ingest meeting-prep
    echo "meetings: prep → feed meeting-prep ($(printf '%s' "$prep" | jq 'length') bullet(s))"
  else
    "$bn" feed clear meeting-prep >/dev/null 2>&1 || true
  fi
else
  echo "meetings: $count meeting(s) → feed; none upcoming → no attention anchor"
  # Nothing upcoming → retire any stale prep so the feed doesn't show a done meeting's brief.
  "$bn" feed clear meeting-prep >/dev/null 2>&1 || true
fi
