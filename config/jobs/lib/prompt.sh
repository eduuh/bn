# Sourced helper for prompt-backed jobs: run a Copilot prompt headlessly with the conventions
# commit-notes proved — bounded context, a timeout, and a graceful fall back when Copilot
# is unavailable. Source it from a job:  . "$(dirname "$0")/lib/prompt.sh"
#
# POSIX sh; never run directly. Every BN_PROMPT_* read is ${X:-default}-guarded so sourcing is
# safe under `set -u`.
#
# Env knobs:
#   BN_PROMPT_AI       "0" disables Copilot (bn_prompt returns nonzero so the caller falls back)
#   BN_PROMPT_MODEL    model for the prompt (default: auto — let Copilot pick)
#   BN_PROMPT_TIMEOUT  seconds before giving up on copilot (default: 60)

# Under a systemd timer / Windows Task Scheduler the login-shell PATH is absent, so copilot
# (~/.local/bin) and bn (~/.bin) aren't found. Prepend them; harmless when already present.
PATH="$HOME/.local/bin:$HOME/.bin:$PATH"
export PATH

. "$(dirname "$0")/lib/realpath.sh"   # bn_realpath — portable `readlink -f`

# Make `bn skills render` find the repo's committed config/prompts/ dir regardless of cwd (cron
# runs in an arbitrary directory). The job lives at <repo>/config/jobs/<job>.sh, so config/prompts/ is one
# sibling below config/. A personal skill in ~/.config/bn/skills still shadows it (global-personal > global-shared).
if [ -z "${BN_MCP_SKILLS_DIR:-}" ]; then
  _bn_job=$(bn_realpath "$0" 2>/dev/null || echo "$0")
  _bn_repo=$(CDPATH= cd "$(dirname "$_bn_job")/../.." 2>/dev/null && pwd)
  if [ -n "$_bn_repo" ] && [ -d "$_bn_repo/config/prompts" ]; then
    BN_MCP_SKILLS_DIR="$_bn_repo/config/prompts"
    export BN_MCP_SKILLS_DIR
  fi
fi

# bn_bound [bytes] — cap stdin to a safe context size (default 6000) for an LLM prompt.
bn_bound() {
  head -c "${1:-6000}"
}

# bn_prompt <prompt-text> — read the context from stdin, fold it into <prompt-text>, run it
# through `copilot -p` and echo the result. Returns nonzero (so the caller can fall back) when
# Copilot is opted out, missing, or produces nothing. `copilot -p` does NOT read stdin, so the
# piped context is embedded into the prompt. Pure text generation: no `--allow-all-tools`, so the
# (untrusted) piped content can never trigger a tool — copilot just answers and exits.
bn_prompt() {
  [ "${BN_PROMPT_AI:-1}" = "0" ] && return 1
  command -v copilot >/dev/null 2>&1 || return 1

  _bn_ctx=$(cat)
  _bn_full=$(printf '%s\n\n--- INPUT ---\n%s\n--- END INPUT ---' "$1" "$_bn_ctx")

  if command -v timeout >/dev/null 2>&1; then
    _bn_out=$(timeout "${BN_PROMPT_TIMEOUT:-60}" copilot -p "$_bn_full" \
      --model "${BN_PROMPT_MODEL:-auto}" --no-color 2>/dev/null)
  else
    _bn_out=$(copilot -p "$_bn_full" \
      --model "${BN_PROMPT_MODEL:-auto}" --no-color 2>/dev/null)
  fi

  [ -n "$_bn_out" ] || return 1
  printf '%s' "$_bn_out"
}
