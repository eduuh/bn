#!/usr/bin/env bash
# voice-check.sh: mechanical AI-tell check for prose.
#
# Usage: voice-check.sh <file> [file...]
#
# Reads the rules in `bn ref writing-voice`. Code blocks and YAML frontmatter are
# stripped before checking, so a command with an em-dash flag or a YAML list is
# never flagged. Line numbers refer to the original file.
#
# FAIL checks are hard: the draft is not done until they are zero. Exit code 1.
# WARN checks need a human call: most hits are real, some are fine.

set -uo pipefail

fail_total=0

# Strip frontmatter and fenced code blocks, keeping original line numbers as "N:".
prose() {
  awk '
    NR == 1 && $0 == "---" { fm = 1; next }
    fm && $0 == "---"      { fm = 0; next }
    fm                     { next }
    /^[[:space:]]*```/     { inblock = !inblock; next }
    inblock                { next }
    # <Terminal lines={[...]} /> is captured output, not prose
    /<Terminal/            { interm = 1 }
    interm                 { if ($0 ~ /\/>/) interm = 0; next }
    /^[[:space:]]*$/       { next }
    { print NR ": " $0 }
  ' "$1"
}

report() { # label, severity, matches
  local label="$1" sev="$2" hits="$3"
  [ -z "$hits" ] && return 0
  local n
  n=$(printf '%s\n' "$hits" | wc -l | tr -d ' ')
  printf '\n  %s  %s (%s)\n' "$sev" "$label" "$n"
  printf '%s\n' "$hits" | sed 's/^/      /' | cut -c1-160
  [ "$sev" = "FAIL" ] && fail_total=$((fail_total + n))
  return 0
}

check_file() {
  local f="$1" p
  p=$(prose "$f")
  printf '\n== %s\n' "$f"

  # ---- hard rules -------------------------------------------------------
  # Frontmatter is stripped from the prose stream, but title / summary / tldr /
  # keyTakeaways all render on the page, so they get their own dash check.
  report "dash in frontmatter, it renders on the page" FAIL \
    "$(awk 'NR == 1 && $0 == "---" { f = 1; next }
            f && $0 == "---" { exit }
            f { print NR ": " $0 }' "$f" | grep -E '—|–')"

  report "em-dash" FAIL "$(printf '%s\n' "$p" | grep '—')"
  report "en-dash in prose" FAIL "$(printf '%s\n' "$p" | grep '–')"
  report "hyphen used as a dash" FAIL \
    "$(printf '%s\n' "$p" | grep -E '[[:alnum:],.")] - [[:alnum:]"(]')"
  report "ellipsis" FAIL "$(printf '%s\n' "$p" | grep -F '...')"
  report "bold-dash bullet stack" FAIL \
    "$(printf '%s\n' "$p" | grep -E '^[0-9]+: *([-*] +)?\*\*[^*]+\*\*[ ]*[-—]')"
  # Title Case, without flagging product names ("Setting up Cloudflare Access").
  # Two tells: a capitalized function word mid-heading, or three capitalized
  # words in a row after the first.
  report "Title Case heading" FAIL \
    "$(printf '%s\n' "$p" | grep -E '^[0-9]+: *#+ ' | grep -E \
      '[[:alnum:]] (With|For|The|In|To|And|Or|Of|A|An|Is|Are|Was|This|That|What|When|Why|How|Not|Use|At|By|On|From|As|It|Its|Your|My|Into|Over|Under|Than|Then)\b|#+ [^ ]+ [A-Z][a-z]+ [A-Z][a-z]+')"
  # a dash in a heading, not a hyphen inside a word (per-repo, 4-port, Pi-hole are fine)
  report "dash in heading, use a colon" FAIL \
    "$(printf '%s\n' "$p" | grep -E '^[0-9]+: *#+ .*(—| - | -$)')"

  # ---- judgement calls --------------------------------------------------
  report "banned vocabulary" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(delve|utilize|facilitate|streamline|elevate|unlock|harness|empower|foster|showcase|spearhead|seamless|robust|comprehensive|cutting-edge|state-of-the-art|best-in-class|crucial|vital|pivotal|intricate|nuanced|holistic|myriad|plethora|deep dive|crown jewel|game-changer|paradigm|realm|tapestry|testament)\b')"

  report "leverage (banned as a verb, fine as a noun)" WARN \
    "$(printf '%s\n' "$p" | grep -iE '\bleverag(e|es|ed|ing)\b')"

  report "scaffolding transition" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(furthermore|moreover|additionally|consequently|notably|importantly|subsequently|therefore|thus|hence|ultimately|overall)\b')"

  report "stock phrase" WARN "$(printf '%s\n' "$p" | grep -iE \
    "(it'?s (important|worth) (to note|noting)|keep in mind|in conclusion|to sum up|let'?s dive|at the end of the day|not only .+ but also|i hope this helps|happy coding|in today'?s|whether you'?re a)")"

  report "antithesis / negative parallelism (one per piece is allowed)" WARN \
    "$(printf '%s\n' "$p" | grep -iE \
    "(isn'?t (just |about |only )?[^.]{2,40}\. it'?s|is not (just|about|only) |not only .+ but |rather than )")"

  # copula avoidance: AI reaches for "serves as" when a human says "is"
  report "copula avoidance, just say is or has" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(serves as|stands as|functions as|acts as|operates as|boasts|offers a|features a|maintains a|represents a|marks the|constitutes)\b')"

  # the significance tail: ", ensuring seamless integration" glued to a real sentence
  report "-ing significance tail" WARN "$(printf '%s\n' "$p" | grep -iE \
    ',[[:space:]]+(highlighting|underscoring|emphasizing|ensuring|reflecting|symbolizing|contributing|cultivating|fostering|encompassing|enhancing|showcasing|demonstrating|solidifying|paving|allowing for|enabling)\b')"

  report "empty intensifier" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(truly|deeply|profoundly|incredibly|remarkably|extremely|highly|significantly|substantially|immensely|vastly)\b')"

  report "hedge sandwich" WARN "$(printf '%s\n' "$p" | grep -iE \
    "while [^.]{5,70}, (it'?s|it is) (worth|important)")"

  report "vague attribution, name it or cut it" WARN "$(printf '%s\n' "$p" | grep -iE \
    "\b(experts (say|argue|agree|note)|studies show|research shows|it'?s widely (known|regarded)|many (believe|argue)|observers|industry reports|some (say|argue|critics))\b")"

  report "words nobody says out loud" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(thereby|wherein|herein|whilst|amongst|aforementioned|heretofore|henceforth|insofar|notwithstanding|endeavour|endeavor|commence|ascertain|requisite|per se|as such|in order to|with regard to)\b')"

  report "there is / there are, give it a real subject" WARN "$(printf '%s\n' "$p" | grep -iE \
    '(^|[.!?] )[[:space:]]*there (is|are|was|were) ')"

  # bolded lead-in lines: a stack of them means the section wanted a table
  local bolds
  bolds=$(printf '%s\n' "$p" | grep -cE '^[0-9]+: *([-*] +)?\*\*')
  if [ "$bolds" -gt 4 ]; then
    printf '\n  WARN  bolded lead-in lines (%s)\n      more than 4 usually means a table or plain prose\n' "$bolds"
  fi

  # one stream of real sentences: paragraph text only, no headings/tables/lists/MDX
  local sentences
  sentences=$(printf '%s\n' "$p" | sed -E 's/^[0-9]+: //' \
    | grep -vE '^[[:space:]]*(#|\||>|[-*+] |[0-9]+\. |<|!\[)' \
    | sed -E 's/([.!?]) +/\1\n/g')

  report "sentence over 25 words, say it in two" WARN \
    "$(printf '%s\n' "$sentences" | awk 'NF > 25 { print NF " words: " substr($0, 1, 120) }')"

  # rhythm. AI writes every sentence the same length. People do not.
  # Calibrated on bits-and-atoms: cv 0.38 to 0.82, median ~0.62. Research puts
  # AI at 0.2 to 0.4 and human prose at 0.6 to 1.2.
  printf '%s\n' "$sentences" | awk '
    NF > 0 { n++; len[n] = NF; sum += NF; if (NF < 8) shortc++; if (NF > 25) longc++ }
    END {
      if (n < 8) { print "\n  INFO  too few sentences to judge rhythm"; exit }
      m = sum / n
      for (i = 1; i <= n; i++) v += (len[i] - m) ^ 2
      sd = sqrt(v / n); cv = sd / m
      printf "\n  RHYTHM  %d sentences, mean %.1f words, sd %.1f, cv %.2f\n", n, m, sd, cv
      if (cv < 0.50)
        printf "      WARN  flat rhythm. Target cv 0.55+. Split a long one, let a short one land.\n"
      if (m > 18)
        printf "      WARN  mean %.1f is long for spoken English. Aim 12 to 15.\n", m
      if (shortc == 0)
        printf "      WARN  no sentence under 8 words. Nothing lands.\n"
      if (longc == 0 && n > 25)
        printf "      INFO  no sentence over 25 words. Fine, but one long one adds contrast.\n"
    }'

  # specificity: AI sounds confident and says nothing. Every section needs a real artifact.
  local vague
  vague=$(printf '%s\n' "$p" | awk -F': ' '
    /^[0-9]+: *## / { if (sec != "" && !hit) print sec; sec = $0; hit = 0; next }
    sec != "" && (/[0-9]/ || /`/) { hit = 1 }
    END { if (sec != "" && !hit) print sec }')
  report "section with no number, command, or filename in it" WARN "$vague"
}

[ $# -eq 0 ] && { echo "usage: voice-check.sh <file> [file...]" >&2; exit 2; }

for f in "$@"; do
  [ -f "$f" ] || { echo "no such file: $f" >&2; exit 2; }
  check_file "$f"
done

printf '\n'
if [ "$fail_total" -gt 0 ]; then
  printf 'FAIL: %s hard-rule hits. Fix these before shipping.\n' "$fail_total"
  exit 1
fi
printf 'PASS: no hard-rule hits. Read the WARN list and judge each one.\n'
