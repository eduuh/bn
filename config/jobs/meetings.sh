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

# 1) Ask workIQ for today's meetings, labelled by importance. Markdown owns the query; the script
#    only supplies runtime date values and routes the result.
question=$("$bn" skills render meetings-query \
  --arg "weekday=$weekday" \
  --arg "date=$today")

wq_timeout="${BN_MEETINGS_WORKIQ_TIMEOUT:-150}"

# 2) Coerce the prose into strict JSON. workiq already judged importance; Copilot only reformats.
#    Reliable links: workIQ rarely has a click-to-join URL, but it DOES attach a per-meeting Teams
#    link as a footnote marker — `[N](https://teams.microsoft.com/l/meeting/details?eventId=…)` —
#    that opens the meeting in Teams (where you join). The notes/footnotes say which [N] is which
#    meeting; the Markdown contract tells Copilot how to preserve that mapping.
schema=$("$bn" skills render meetings-schema)

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
  prep_q=$("$bn" skills render meeting-prep-query \
    --arg "subject=$subject" \
    --arg "start=$start")
  prep_schema=$("$bn" skills render meeting-prep-schema)
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
