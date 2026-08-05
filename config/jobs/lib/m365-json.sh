# Sourced helper for Microsoft 365 jobs: ask WorkIQ, normalize its prose to a JSON array through
# the local Copilot prompt helper, and fall back to raw JSON if Copilot is unavailable.
# Source it from a job:  . "$(dirname "$0")/lib/m365-json.sh"
#
# POSIX sh; never run directly. Safe under `set -u`.

. "$(dirname "$0")/lib/prompt.sh"   # bn_prompt / bn_bound — local Copilot normalizer

# bn_prompt_json <schema> [bytes] — read an answer from stdin and echo a compact JSON array.
# Returns nonzero when neither the Copilot-normalized response nor the raw input is a JSON array.
bn_prompt_json() {
  _bn_prompt_json_schema=$1
  _bn_prompt_json_bytes=${2:-16000}
  _bn_prompt_json_answer=$(cat)

  _bn_prompt_json_out=$(printf '%s' "$_bn_prompt_json_answer" | bn_bound "$_bn_prompt_json_bytes" | bn_prompt "$_bn_prompt_json_schema" 2>/dev/null \
    | sed -e 's/^```json//' -e 's/^```//' -e 's/```$//' \
    | jq -c 'if type=="array" then . else empty end' 2>/dev/null || true)
  [ -z "$_bn_prompt_json_out" ] && _bn_prompt_json_out=$(printf '%s' "$_bn_prompt_json_answer" | jq -c 'if type=="array" then . else empty end' 2>/dev/null || true)

  [ -n "$_bn_prompt_json_out" ] || return 2
  printf '%s\n' "$_bn_prompt_json_out"
}

# m365_query_json <timeout-seconds> <question> <schema> [bytes] — ask WorkIQ and normalize to JSON.
# Return 1 for a timeout/empty WorkIQ answer, 2 for parse failure; echo the JSON array on success.
m365_query_json() {
  _bn_m365_timeout=$1
  _bn_m365_question=$2
  _bn_m365_schema=$3
  _bn_m365_bytes=${4:-16000}

  # `</dev/null`: workiq reads stdin and, with no TTY (cron/non-interactive runs), blocks on it
  # until the timeout fires unless stdin is closed. `timeout` is GNU-only (absent on stock macOS),
  # so guard it and fall back to an unbounded call — matching lib/prompt.sh.
  if command -v timeout >/dev/null 2>&1; then
    _bn_m365_answer=$(timeout "$_bn_m365_timeout" workiq ask -q "$_bn_m365_question" </dev/null 2>/dev/null || true)
  else
    _bn_m365_answer=$(workiq ask -q "$_bn_m365_question" </dev/null 2>/dev/null || true)
  fi
  [ -n "$_bn_m365_answer" ] || return 1

  printf '%s' "$_bn_m365_answer" | bn_prompt_json "$_bn_m365_schema" "$_bn_m365_bytes"
}
