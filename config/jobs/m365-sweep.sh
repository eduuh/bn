#!/bin/sh
# m365-sweep.sh — one batched pass over Microsoft 365 (via the `workiq` CLI) for everything that
# needs you, routed to bn's surfaces. Same pipeline as meetings.sh — workIQ answers, a shared local
# Copilot pass (lib/m365-json.sh) normalizes the prose to JSON, jq fans it out — run once across
# four signals so workIQ's latency is paid a single time:
#
#   1. urgent emails      → attention (reply-needed) + feed `email` (all worth seeing)
#   2. Teams @-mentions   → attention (reply-needed) + feed `teams`
#   3. today's commitments→ daily goals (today's plan)
#   4. dropped balls       → feed `owe` (glance only — I owe someone a reply)
#
# Conservative loudness: an attention item is `urgent` (and so toasts via the notify job) ONLY when
# workIQ is confident it's high-stakes — your manager/skip-level wrote, a deadline is today, or it's
# a direct question to you. Everything else is `warn`: visible in `bn next`, silent.
#
# Best-effort and non-destructive: each section is independent; a workIQ stall or empty answer for
# one leaves that surface as-is (keeps the last good data) and the others still run. workIQ lives on
# the work laptop only, so this is a clean no-op elsewhere — pair with `[jobs.only_on] m365-sweep =
# ["<laptop>"]`. Triggered by `bn run m365-sweep`; scheduled afternoon + catch_up (first bn event).
set -eu

bn="${BN:-$HOME/.config/bn/bn}"
. "$(dirname "$0")/lib/m365-json.sh"   # m365_query_json — WorkIQ + local Copilot JSON normalizer

command -v workiq >/dev/null 2>&1 || { echo "m365-sweep: workiq not found — skipping" >&2; exit 0; }
command -v jq     >/dev/null 2>&1 || { echo "m365-sweep: jq not found" >&2; exit 0; }

today=$(date +%F)
wq_timeout="${BN_M365_WORKIQ_TIMEOUT:-150}"

# ask_json <question> <schema> — echo "" on any stall/empty/parse failure so the caller can skip
# that section without touching its surface.
ask_json() {
  m365_query_json "$wq_timeout" "$1" "$2" || true
}

# clear_source <source> — ack every open attention item bn currently holds for this source, so the
# re-add below leaves exactly the live set (replied/handled items drop off on their own).
clear_source() {
  "$bn" attn list --json 2>/dev/null \
    | jq -r --arg s "$1" '.data.items[]? | select(.source==$s) | .id' 2>/dev/null \
    | while IFS= read -r _id; do [ -n "$_id" ] && "$bn" attn ack "$_id" >/dev/null 2>&1 || true; done
}

# route_attn <source> — read a JSON array on stdin of {key,title,link,urgent} and raise one
# attention item per element. `urgent:true` → severity urgent (toasts); else warn (silent).
route_attn() {
  _src=$1
  jq -c '.[]' | while IFS= read -r _it; do
    _title=$(printf '%s' "$_it" | jq -r '.title // empty')
    [ -z "$_title" ] && continue
    _link=$(printf '%s' "$_it" | jq -r '.link // ""')
    _key=$(printf '%s'  "$_it" | jq -r '.key // empty')
    _sev=$(printf '%s'  "$_it" | jq -r 'if (.urgent // false) then "urgent" else "warn" end')
    # `|| true`: a failed add must not abort the run — this loop is the last stage of the caller's
    # pipe, so under `set -e` its exit status would propagate and kill the remaining sections.
    "$bn" attn add "$_title" --severity "$_sev" --source "$_src" \
      ${_key:+--key "$_src-$today-$_key"} ${_link:+--link "$_link"} >/dev/null || true
  done
}

# ── 1) Urgent emails ─────────────────────────────────────────────────────────────────────────────
email_q=$("$bn" skills render m365-email-query)
email_schema=$("$bn" skills render m365-email-schema)
emails=$(ask_json "$email_q" "$email_schema")
if [ -n "$emails" ] && [ "$(printf '%s' "$emails" | jq 'length')" -gt 0 ]; then
  printf '%s' "$emails" | jq -c '.[] | {key:.id, text:.title, url:(.link//"")}' | "$bn" feed ingest email
  clear_source email
  printf '%s' "$emails" | route_attn email
  echo "m365-sweep: $(printf '%s' "$emails" | jq 'length') email(s) → feed + attention"
else
  echo "m365-sweep: no actionable emails — leaving email surfaces as-is"
fi

# ── 2) Teams @-mentions / DMs awaiting a reply ───────────────────────────────────────────────────
teams_q=$("$bn" skills render m365-teams-query)
teams_schema=$("$bn" skills render m365-teams-schema)
mentions=$(ask_json "$teams_q" "$teams_schema")
if [ -n "$mentions" ] && [ "$(printf '%s' "$mentions" | jq 'length')" -gt 0 ]; then
  printf '%s' "$mentions" | jq -c '.[] | {key:.id, text:.title, url:(.link//"")}' | "$bn" feed ingest teams
  clear_source teams
  printf '%s' "$mentions" | route_attn teams
  echo "m365-sweep: $(printf '%s' "$mentions" | jq 'length') Teams mention(s) → feed + attention"
else
  echo "m365-sweep: no Teams mentions awaiting me — leaving teams surfaces as-is"
fi

# ── 3) Today's commitments / action items → daily goals ──────────────────────────────────────────
commit_q=$("$bn" skills render m365-commitments-query)
commit_schema=$("$bn" skills render m365-commitments-schema)
commitments=$(ask_json "$commit_q" "$commit_schema")
if [ -n "$commitments" ] && [ "$(printf '%s' "$commitments" | jq 'length')" -gt 0 ]; then
  # Dedup against whatever text is already on today's list (recursive text scan = shape-agnostic).
  existing=$("$bn" daily --json 2>/dev/null | jq -r '[.. | .text? // empty] | .[]' 2>/dev/null || true)
  printf '%s' "$commitments" | jq -c '.[]' | while IFS= read -r c; do
    text=$(printf '%s' "$c" | jq -r '.text // empty'); [ -z "$text" ] && continue
    # Whole-LINE match (newlines wrap both sides), not a substring — else a short task that's a
    # substring of an existing goal would be wrongly skipped.
    case "
$existing
" in *"
$text
"*) continue ;; esac
    link=$(printf '%s' "$c" | jq -r '.link // ""')
    "$bn" daily add "$text" ${link:+--link "$link"} >/dev/null || true
  done
  echo "m365-sweep: $(printf '%s' "$commitments" | jq 'length') commitment(s) → daily goals"
else
  echo "m365-sweep: no new commitments — leaving daily goals as-is"
fi

# ── 4) Dropped balls — I owe a reply (glance only, never attention) ───────────────────────────────
owe_q=$("$bn" skills render m365-owed-query)
owe_schema=$("$bn" skills render m365-owed-schema)
owe=$(ask_json "$owe_q" "$owe_schema")
if [ -n "$owe" ] && [ "$(printf '%s' "$owe" | jq 'length')" -gt 0 ]; then
  printf '%s' "$owe" \
    | jq -c 'sort_by(-(.days // 0))[]
             | {key:(.id // .who), text:(((.days // 0)|tostring) + "d  " + (.who // "?") + " — " + (.topic // "")),
                url:(.link//"")}' \
    | "$bn" feed ingest owe
  echo "m365-sweep: $(printf '%s' "$owe" | jq 'length') owed repl(y/ies) → feed"
else
  echo "m365-sweep: nothing owed — leaving owe feed as-is"
fi
