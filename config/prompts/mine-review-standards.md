---
name: mine-review-standards
description: Mine real reviewer comments from a repo's merged PRs and distill them into a bn review-standards reference
when: Bootstrapping or refreshing a review specialist's standards from what humans actually flag in PRs (via `bn agents bootstrap`).
keywords: [review, standards, pull request, ado, azure devops, mining, reviewer comments, specialist, bootstrap]
arguments:
  - { name: repo, description: "bn repo the specialist reviews (names the reference: <repo>-review-standards)", required: false }
  - { name: source_repo, description: "Azure DevOps repo whose merged-PR comments to mine", required: false }
  - { name: ref, description: "Reference doc name to write (default: <repo>-review-standards)", required: false }
---
You are mining **real human reviewer comments** from an Azure DevOps repo's merged pull requests
and distilling them into a reusable bn *review-standards* reference — the knowledge a review
specialist loads. This is READ-ONLY against ADO; the only thing you write is one bn reference doc.

## Parameters
- Target repo: `{{repo}}` — the bn repo the specialist reviews.
- Source repo (ADO): `{{source_repo}}` — where the comments come from.
- Reference to write: `{{ref}}` (default `{{repo}}-review-standards`).

If any parameter is empty (e.g. you were **dispatched as a separate agent**), read them from your
task goal — it names the target repo, the ADO source repo, and the reference doc to write.

## Steps

1. **Fetch human reviewer comments.** Run the helper — it paginates the most-recent *completed*
   PRs (default 200; raise `--max` for a bigger starting corpus) and extracts human `text`
   comments, dropping system threads and obvious bots. It is strictly READ-ONLY against ADO:
   ```bash
   config/scripts/ado-pr-comments.sh --source <source_repo> --max 200 > /tmp/pr-comments.json
   # optional: --org <org|url> --project <project>   # default to your `az devops configure`
   ```
   Get **as many comments as you can** — a larger corpus makes a stronger starting standard.
   Call it by its full repo path if it isn't on `$PATH`. If it exits non-zero (no `az`, not
   logged in, unresolved org/project, unknown repo), **STOP and report exactly what failed and
   how to fix it** — never invent standards from nothing.

2. **Keep only genuine human review feedback.** The helper is a coarse filter; you are the smart
   one. From `/tmp/pr-comments.json`, discard anything automated that slipped through — bot/agent
   banners (e.g. "SydGate Agent", policy/guard bots, "Review complete for PR"), pure status
   chatter ("LGTM", "done", "thanks"), and off-topic noise. Keep substantive comments where a
   reviewer asked for a change or flagged a concern.

3. **Cluster into recurring themes** — the things reviewers *here* repeatedly flag: correctness &
   edge cases, error handling, test coverage, API/contract misuse, performance, security &
   privacy/compliance, logging/telemetry, naming & readability, and codebase-specific nits. Aim
   for 8–20 concrete, actionable bullets grouped under short headings. Ground each in the data —
   paraphrase a representative comment and cite its PR id — so a standard is specific, not generic.

4. **Write the reference, preserving the user's own additions:**
   - Read the current doc: `bn ref {{ref}}` (may be empty or absent).
   - Everything under a `## My additions` heading is **USER-OWNED** — never edit, reorder, or drop
     it. If the doc has no such section, add an empty one so the user has a home for hand edits.
   - Scaffold the file if new: `bn ref add {{ref}}` (it prints the file path). Then write the
     merged content to that file directly with your editor tools — regenerate only the mined
     section, and re-attach the preserved `## My additions` verbatim.
   - Use this structure:
     ```
     # {{ref}}

     _Mined from the N most-recent merged PRs of <source_repo> on <date>._

     ## What reviewers flag here

     ### <theme>
     - <standard>. _(e.g. "<short paraphrase>", PR #<id>)_
     ...

     ## My additions

     <preserved verbatim — do not touch>
     ```

5. **Report** in 2–4 lines: how many PRs/comments you mined, how many themes you distilled, the
   reference path written, and remind the user to run **`bn agents install`** so the specialist
   recompiles with the fresh standards.

Never post anything back to ADO. If you mined zero usable human comments, say so plainly and leave
any existing `## My additions` untouched rather than writing an empty standards section.
