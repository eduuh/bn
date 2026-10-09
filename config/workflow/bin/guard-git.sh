#!/usr/bin/env bash
# guard-git.sh — deny destructive git commands from Copilot (preToolUse) and Claude (PreToolUse).
#
# Reads the hook payload on stdin. Copilot sends {toolName, toolArgs}; Claude sends
# {tool_name, tool_input}. Denies force-push, reset --hard, branch -D, clean -f and bare stash.
# Escape hatch: BN_GUARD_OFF=1 (bn pool cleanup legitimately resets worktrees).
set -u

[ "${BN_GUARD_OFF:-}" = 1 ] && exit 0
command -v jq >/dev/null 2>&1 || exit 0

payload=$(cat)
cmd=$(printf '%s' "$payload" | jq -r '
  (.tool_input.command // (.toolArgs | if type == "string" then (try fromjson catch {}) else . end | .command) // empty)' 2>/dev/null)
[ -n "$cmd" ] || exit 0

reason=""
if printf '%s' "$cmd" | grep -Eq '(^|[;&|[:space:]])git[[:space:]]+([^;&|]*[[:space:]])?push([[:space:]].*)?[[:space:]](-f|--force|--force-with-lease(=[^[:space:]]*)?|--force-if-includes)([[:space:]]|$)'; then
  reason="force-push"
elif printf '%s' "$cmd" | grep -Eq '(^|[;&|[:space:]])git[[:space:]]+([^;&|]*[[:space:]])?push([[:space:]].*)?[[:space:]]\+[^[:space:]]'; then
  reason="force-push (+refspec)"
elif printf '%s' "$cmd" | grep -Eq '(^|[;&|[:space:]])git[[:space:]]+([^;&|]*[[:space:]])?reset([[:space:]].*)?[[:space:]]--hard'; then
  reason="reset --hard"
elif printf '%s' "$cmd" | grep -Eq '(^|[;&|[:space:]])git[[:space:]]+([^;&|]*[[:space:]])?branch([[:space:]].*)?[[:space:]]-D([[:space:]]|$)'; then
  reason="branch -D"
elif printf '%s' "$cmd" | grep -Eq '(^|[;&|[:space:]])git[[:space:]]+([^;&|]*[[:space:]])?clean([[:space:]].*)?[[:space:]]-[a-z]*f'; then
  reason="clean -f"
elif printf '%s' "$cmd" | grep -Eq '(^|[;&|[:space:]])git[[:space:]]+([^;&|]*[[:space:]])?stash([[:space:]]+(push|pop|drop|clear|apply))?[[:space:]]*($|[;&|])'; then
  reason="bare git stash (shared across worktrees)"
fi
[ -n "$reason" ] || exit 0

msg="Blocked by bn guard: $reason. Ask the user, or set BN_GUARD_OFF=1 for an approved run."
if printf '%s' "$payload" | jq -e 'has("toolName")' >/dev/null 2>&1; then
  jq -n --arg r "$msg" '{permissionDecision: "deny", permissionDecisionReason: $r}'
else
  jq -n --arg r "$msg" '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: $r}}'
fi
exit 0
