#!/bin/sh
# Keep static AI role/process/output instructions in config/agents or config/prompts.
set -eu

failed=0

report_hits() {
  label=$1
  hits=$2
  if [ -n "$hits" ]; then
    printf '%s\n%s\n' "$label" "$hits" >&2
    failed=1
  fi
}

rust_hits=$(
  find crates/bn-core/src crates/bn-cli/src crates/bn-web/src \
    -type f -name '*.rs' ! -name 'tests.rs' ! -path '*/tests/*' \
    -exec grep -nHE \
      'You are |Act as |Your task is|Output (ONLY|only)|Return (ONLY|only|exactly)|Respond (ONLY|only)|Write no code yourself' \
      {} + 2>/dev/null || true
)
report_hits \
  "Static AI instructions found in production Rust; move them to config/agents or config/prompts:" \
  "$rust_hits"

for file in config/jobs/*.sh; do
  [ -f "$file" ] || continue

  shell_hits=$(
    grep -nE \
      'You are |Act as |Your task is|Output (ONLY|only)|Return (ONLY|only|exactly)|Respond (ONLY|only)|Write no code yourself|(^|[[:space:]])([[:alnum:]_]+_(q|schema)|question|schema)=["'\'']' \
      "$file" 2>/dev/null || true
  )
  report_hits \
    "Static AI instructions found in scheduled shell job $file; render Markdown instead:" \
    "$shell_hits"

  if grep -Eq 'agency[[:space:]]+copilot|m365_query_json|bn_prompt' "$file"; then
    if ! grep -Eq '^[[:space:]]*[^#].*(agents|skills)[[:space:]]+render' "$file"; then
      printf '%s\n' \
        "Prompt-backed shell job $file does not call 'bn agents render' or 'bn skills render'." >&2
      failed=1
    fi
  fi

  if grep -Eq '^[[:space:]]*[^#].*agency[[:space:]]+copilot' "$file"; then
    if ! grep -Eq '^[[:space:]]*[^#].*agents[[:space:]]+render' "$file"; then
      printf '%s\n' \
        "Agent-backed shell job $file must obtain its role prompt with 'bn agents render'." >&2
      failed=1
    fi
  fi
done

if [ "$failed" -ne 0 ]; then
  exit 1
fi

printf '%s\n' "prompt-source guard: ok"
