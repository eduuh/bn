#!/bin/sh
# meetings.sh — surface today's meetings that actually matter. Microsoft 365 Copilot (the
# `workiq` CLI) holds the calendar and judges importance; bn renders. Two surfaces, mirroring the
# README's feed/attention split:
#
#   - the FEED gets every important meeting (`bn feed ingest meetings`). Each run *replaces* the
#     source, so finished meetings fall off on their own — no expiry logic here.
#   - the GAP CACHE gets the day's events, so `bn gap` / `bn next --fit` see the calendar.
#   - the FEED source `meeting-prep` gets a short brief for the next important meeting (agenda,
#     your role, the latest thread, a doc to skim) — one extra workIQ call, only when one's
#     upcoming. Attention items are NOT raised here: the consolidated attention-sweep owns the
#     attention list, so a meeting never lands twice under two sources.
#
# Pipeline: workiq answers (and judges which meetings are skippable) → a shared local Copilot pass
# (lib/m365-json.sh) coerces that prose into strict JSON → jq fans it out to the surfaces.
#
# Best-effort and non-destructive: if workiq is absent/unauthenticated or yields nothing, the
# feed and attention list are left as-is rather than cleared. workiq lives only on the work
# laptop, so this is a clean no-op elsewhere — pair with `[jobs.only_on] meetings = ["<laptop>"]`
# to keep the timer off other machines too. Triggered by `bn run meetings`; scheduled by the
# meetings agent manifest (config/agents/meetings.md, weekdays 14:50, `catch_up: true` — so it
# also re-fires on the day's first bn event via `bn run meetings --if-stale`).
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
# No explicit bytes bound: bn_prompt_json's 16000 default is ample for the ~300-char footnote
# join URLs we need preserved.
meetings=$(m365_query_json "$wq_timeout" "$question" "$schema") || meetings_rc=$?
if [ "$meetings_rc" -eq 1 ]; then
  echo "meetings: workiq returned nothing within ${wq_timeout}s — leaving feed/attention as-is"
  exit 0
fi
if [ "$meetings_rc" -ne 0 ]; then
  echo "meetings: could not parse workiq's answer — leaving feed/attention as-is"
  exit 0
fi

count=$(printf '%s' "${meetings:-}" | jq 'length' 2>/dev/null || echo 0)
if [ "${count:-0}" -eq 0 ]; then
  # A *parsed* empty array is a confident all-clear (distinct from the stall/parse cases above):
  # drop any stale meeting anchor so a no-meeting day doesn't keep nagging with yesterday's top.
  "$bn" attn list --json 2>/dev/null \
    | jq -r '.data.items[]? | select(.source=="meetings") | .id' 2>/dev/null \
    | while IFS= read -r id; do [ -n "$id" ] && "$bn" attn ack "$id" >/dev/null 2>&1 || true; done
  echo "meetings: no important meetings for $today — cleared any stale meeting attention"
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

# 4) NEXT-UP — attention is attention-sweep's job now (the consolidated gatherer), so no anchor
#    is raised here; the clear below is retirement hygiene draining anything the old behavior
#    left under this source. The most-important upcoming meeting is still computed: it picks
#    which meeting gets the prep brief in step 5.
"$bn" attn list --json 2>/dev/null \
  | jq -r '.data.items[]? | select(.source=="meetings") | .id' 2>/dev/null \
  | while IFS= read -r id; do [ -n "$id" ] && "$bn" attn ack "$id" >/dev/null 2>&1 || true; done

now=$(date +%H:%M)
# The most important meeting that's both still ahead AND genuinely worth preparing for
# (critical/high). Missing start → treated as upcoming (surface, not drop). The feed keeps
# showing the whole day regardless. Highest rank wins; among equals, the earliest start.
top=$(printf '%s' "$meetings" | jq -c --arg now "$now" '
  def rank: {"critical":3,"high":2,"medium":1,"low":0}[(.importance // "" | ascii_downcase)] // 0;
  [ .[] | select((.start // "99:99") >= $now and rank >= 2) ]
  | sort_by([-(rank), (.start // "99:99")]) | .[0] // empty')

if [ -n "$top" ]; then
  subject=$(printf '%s' "$top" | jq -r '.subject')
  start=$(printf '%s'   "$top" | jq -r '.start // ""')
  echo "meetings: $count meeting(s) → feed; next-up: $start $subject"

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
