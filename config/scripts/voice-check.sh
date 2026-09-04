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
    # An unterminated JSX open tag starts a multi-line props/data block:
    # <Terminal lines={[…]} /> is captured output, <Sandpack files={{…}} /> is
    # real source. Both are code, not prose. A tag that closes on its own line
    # (<Callout type="tip">, a one-line <GitHubRepo … />) still gets checked.
    /^[[:space:]]*<[A-Z][A-Za-z]*/ && $0 !~ />[[:space:]]*$/ { injsx = 1 }
    injsx                  { if ($0 ~ /\/>/) injsx = 0; next }
    /^[[:space:]]*$/       { next }
    # blank out inline `code` spans: `{...}` and `bs=1m --flag` are not prose
    { line = $0; gsub(/`[^`]*`/, "``", line); print NR ": " line }
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
  report "curly quote, use a straight one" FAIL \
    "$(printf '%s\n' "$p" | grep $'[‘’“”]')"
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
    '\b(delve|utilize|facilitate|streamline|elevate|unlock|harness|empower|foster|showcase|spearhead|embark|underscore|garner|bolster|enhance|exemplify|seamless|robust|comprehensive|cutting-edge|state-of-the-art|best-in-class|world-class|elegant|crucial|vital|pivotal|intricate|nuanced|holistic|myriad|plethora|meticulous|vibrant|valuable|enduring|groundbreaking|renowned|profound|deep dive|crown jewel|game-changer|paradigm|realm|journey|tapestry|testament|interplay|focal point|diverse array)\b')"

  # "key" is a noun and a keyboard key all over the corpus. Only the adjective is banned.
  report "key as an adjective" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\bkey (takeaways?|features?|benefits?|advantages?|components?|aspects?|elements?|factors?|considerations?|differentiators?|capabilit(y|ies))\b')"

  # abstract metaphor nouns: read as technical, mean almost nothing.
  # "primitive" and "surface" are left out on purpose: "Kubernetes storage primitives"
  # and "attack surface" are real names for real things in this corpus.
  report "abstract metaphor noun, pick the concrete word" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(substrate|locus|vantage|nexus|bedrock|modality|flywheel|north star|endgame|gold-plat(e|ed|ing)|ratchet|wedge (in|it|them)|evacuat(e|ed|ing)|attack vector|threat vector|growth vector)\b')"

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
    '\b(serves as|stands as|functions as|acts as|operates as|boasts|offers a|features a|provides a|maintains a|represents a|marks the|constitutes)\b')"

  # "**Performance:** Performance improved." The bold label is fine; restating it is the tell.
  # A bold label that carries new detail after it ("**Schema in TypeScript.** Tables live in
  # one file.") is not a tell, so only a repeated *content* word counts, and only for a label
  # short enough to be an inline header rather than a whole bolded sentence.
  report "bold lead-in the line then restates" WARN "$(printf '%s\n' "$p" | awk '
    BEGIN { split("the a an it this that and but so i you we my your there here of to is are", s)
            for (i in s) stop[s[i]] = 1 }
    match($0, /\*\*[^*]+\*\*:?/) {
      label = substr($0, RSTART + 2, RLENGTH - 4)
      sub(/:$/, "", label)
      rest = substr($0, RSTART + RLENGTH)
      sub(/^[[:space:]:.]+/, "", rest)
      if (split(label, lw, /[^[:alnum:]]+/) > 5) next
      split(rest, rw, /[^[:alnum:]]+/)
      w = tolower(lw[1])
      if (w != "" && !stop[w] && w == tolower(rw[1])) print
    }')"

  # the significance tail: ", ensuring seamless integration" glued to a real sentence
  report "-ing significance tail" WARN "$(printf '%s\n' "$p" | grep -iE \
    ',[[:space:]]+(highlighting|underscoring|emphasizing|ensuring|reflecting|symbolizing|contributing|cultivating|fostering|encompassing|enhancing|showcasing|demonstrating|solidifying|paving|allowing for|enabling)\b')"

  report "empty intensifier" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(truly|deeply|profoundly|incredibly|remarkably|extremely|highly|significantly|substantially|immensely|vastly)\b')"

  # an adverb propping up a weak verb. The number belongs there instead.
  report "adverb doing the verb's job, give the number" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(dramatically|drastically|considerably|markedly|greatly|massively|hugely|enormously|effortlessly|painlessly|flawlessly|blazingly)\b')"

  # passive voice. Idiomatic participles are excluded; the rest usually have a
  # knowable actor that the sentence is hiding.
  report "passive voice, name who does it" WARN "$(printf '%s\n' "$p" \
    | grep -iE '\b(is|are|was|were|been|being) (also |already |still |now |never |only |always |then )?[a-z]+ed\b' \
    | grep -ivE '\b(is|are|was|were|been|being) (also |already |still |now |never |only |always |then )?(used|based|called|named|supposed|related|interested|tired|involved|limited|located|excited|surprised|pleased|finished|allowed|expected|needed|required|deprecated|deleted|installed)\b')"

  report "filler phrase" WARN "$(printf '%s\n' "$p" | grep -iE \
    '\b(due to the fact that|in the event that|at this point in time|for the purpose of|in spite of the fact|has the ability to|a number of|the majority of|it is important to)\b')"

  # assistant voice leaking into a PR description, an issue comment or an email
  report "chatbot artifact" WARN "$(printf '%s\n' "$p" | grep -iE \
    "(great question|good catch|absolutely right|you'?re (absolutely )?right|certainly!|of course!|perfect!|smoking gun|let me know if|feel free to|i'?d be happy to|based on the information available|while specific details are limited)")"

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
