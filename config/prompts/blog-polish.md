---
name: blog-polish
description: "Final voice and style check for a drafted blog post. Use once after drafting or revising a blog post, never for other prose."
when: A blog post draft or revision is ready for its final voice and style check.
keywords: [blog, blog-post, blog-polish, blog-editing]
---

# blog-polish (final-pass verification)

This skill runs as a **final verification pass** for blog posts only, not as a style guide loaded up front. Draft or revise the blog post first. When the post is otherwise done, load this skill once and sweep only that post for AI tells and voice misses.

The point is to keep context lean during the task and apply the heavy rules in one targeted sweep at the end.

## When to invoke

Invoke **once, near end of task**, when both are true:

1. The task drafted or revised a blog post.
2. The post is otherwise done and saved, ready for its final voice and style check.

**Don't invoke** in these cases:
- Pure code changes, configs, JSON, command output, structured data.
- Mid-task. Write the prose first, polish at the end.
- The task did not draft or revise a blog post.
- READMEs, docs, commits, PR descriptions, branch notes, emails, replies and code comments, even inside a blog-writing task.

If the task produces multiple blog posts, run the sweep once per post.

## How to run the sweep

Treat this like a linter pass. Steps:

1. **List the blog drafts touched.** Only blog posts written or edited in this task.
2. **Re-read each one.** Don't trust memory.
3. **Run the mechanical checks below.** Every grep that comes back non-empty is a fix, not a judgement call.
4. **Read aloud in your head.** Anywhere it sounds like a corporate slide, help-desk article, polite LinkedIn post, or textbook → rewrite that part.
5. **Check rhythm.** Three same-length sentences in a row? Break one. Same opening word twice? Vary.
6. **Confirm one human moment.** Specific number, specific name, first-person reaction, cost, opinion. If none, add one.
7. **Apply fixes** with edits. Don't summarise the changes back at the user — just make them.
8. **Report counts, not vibes.** Required final report shape: list every mechanical check with its result (pass / list of fixes applied). Without those numbers, you didn't run the sweep.

## Mechanical checks (run these, fix every hit)

These are non-negotiable. Run each grep on the file. If it returns anything, fix it. If you think you have a justification for leaving a hit, see "Stop rationalizing" below — your justification is almost certainly one of the listed excuses.

Set `FILE` to the path you're polishing.

### 1. Banned-words grep — must be empty

```bash
grep -inE "delve|tapestry|leverage|utilize|seamless|robust|cutting-edge|paradigm|journey|ecosystem|landscape|myriad|plethora|vibrant|boast|bolster|crucial|pivotal|underscore|testament|aligns?|resonate|enhance|showcase|highlight|emphasize|encompass|cultivat|garner|interplay|nestled|renowned|profound|commitment to|diverse array|furthermore|moreover|additionally|in conclusion|to summarize|game-changer|state-of-the-art|stands as|serves as|marks a|represents a|in the heart of|setting the stage|paving the way|not just .+ (it.s|but)|in today.s|in a world|interestingly|essentially|basically" "$FILE"
```

### 2. Inline-header bullet stack — at most 1 in the whole post

```bash
grep -cE '^[[:space:]]*[-*][[:space:]]+\*\*[^*]+\*\*[[:space:]]*[:.-]' "$FILE"
```

If the count is **2 or more**, convert to prose. The pattern is the AI list shape. The fix is always the same: turn it into one or two sentences. "You'll need X (for Y), Z, and W." beats four bolded bullets every time.

The only legitimate exception is a literal data table, which should be a markdown table (`|...|`), not a bullet list with bolded mini-headers.

### 3. Em-dash in headings — must be empty

```bash
grep -nE '^#+ .*—' "$FILE"
```

Headings stay clean. If the heading wants a colon clause, use `:` not `—`. If it wants a sub-thought, drop the sub-thought and put it in the prose.

### 4. Em-dash density per paragraph — at most 1

```bash
awk 'BEGIN{RS=""} {n=gsub(/—/,"&"); if(n>1) print NR": "n" em-dashes"}' "$FILE"
```

Any paragraph with 2+ em-dashes gets one rewritten as a period, comma, or parenthesis.

### 5. Bolds — at most 1 per `##` section

```bash
awk '/^## /{if(s!=""&&c>1)print s": "c" bolds"; s=$0; c=0} /\*\*[^*]+\*\*/{c+=gsub(/\*\*[^*]+\*\*/,"&")} END{if(c>1)print s": "c" bolds"}' "$FILE"
```

If a section has more than one bold, drop all but the most load-bearing one. Bolds should be reserved for the principle the section is built on.

### 6. Throat-clearing fragments — must be empty

```bash
grep -inE "That.s the whole|The trick is that|It.s worth noting|Here.s the thing|What.s interesting is|This post (walks|covers|explains|will|shows you)|In this post|Let.s (dive|explore|take a look|walk through)" "$FILE"
```

These are post-meta — the post talking about itself. Cut them. The prose either lands directly or it doesn't.

### 7. Trailing `-ing` participles — audit

```bash
grep -nE "ing( its| their| his| her| the)? (impact|significance|community|importance|appeal|legacy|presence)\.\s*$" "$FILE"
```

If any sentence ends with this shape, rewrite it. End on the concrete fact, not a participle dressing.

### 8. Curly quotes / apostrophes — must be empty

```bash
grep -nE '[\xe2\x80\x98\xe2\x80\x99\xe2\x80\x9c\xe2\x80\x9d]' "$FILE"
```

Replace with straight `'` and `"`.

### 9. Title-case headings — must be empty

```bash
grep -nE '^#+ [A-Z][a-z]+ [A-Z][a-z]+ [A-Z][a-z]+' "$FILE"
```

Headings are sentence case. Lowercase everything except proper nouns and the first word.

## Stop rationalizing

When applying the rules, these are the excuses that surface. None of them are valid.

- **"But Edd uses this pattern in [other post]."** No. The rules apply to *this* post. If you find a counter-example in another post, it's either a missed lint or a pattern that earned its keep there in a way it doesn't here. Default to the rule.
- **"It's a literal X, so the triad is OK."** Triads are OK only when each item is genuinely distinct *and* none of the three is rhythm padding. If you can drop one without losing meaning, drop it. If you genuinely have three concrete things, you're fine — but verify by reading the sentence with the third item removed.
- **"5 inline-header bullets is OK because it's a prereq list."** No. Convert to prose. "You'll need X, Y, and Z. The optional pieces are A and B." A real data table is a markdown table, not bolded bullets.
- **"The em-dash in the heading adds personality."** No. It adds clutter. Personality goes in the prose.
- **"The reader benefits from the structure."** Bullets and tables are for *data*, not for chunked sentences. If your bullets read as sentences with a bolded keyword on the front, they're prose in disguise.
- **"This bold is important."** Maybe. But you have three bolded things in this section. Pick one. The principle the section is built on. The other two become plain text or `inline code`.
- **"It's just a small thing, the post is otherwise clean."** The small things are the watermarks. A reader can't articulate what feels off, but they feel it. Fix the small thing.

If you find yourself reaching for any of these, you're failing the sweep. Treat the mechanical checks as hard gates: every hit must result in either a fix or a documented exception with the reason logged in the report.

## Scope tuning

Different surfaces have different ceilings. The voice is the same, the length and formality shift:

| Surface | Length | Cursing | Signature phrases |
|---------|--------|---------|-------------------|
| Blog post | Long-form | Light, occasional | Yes |
| README / docs | Medium | Avoid | Sparingly |
| PR description | Short | Avoid | Sparingly |
| Commit message | One line | No | No |
| Branch note | Short bullets | OK | OK |
| Slack / chat | Short | OK in context | Yes |
| CLI reply (this session) | Short | OK if it fits | Yes |

## How Edd actually writes (observed)

These are patterns from watching him in action, not stated rules. Match them.

### His chat style is fast and lowercase

His own messages skip capitalization, leave typos in ("Godamit", "cally", "ny form"), and don't bother with polite preamble. They're short. They're direct. He trusts the reader.

When mirroring his style in chat or replies: stay short. Don't over-format. Don't open with "Sure!" or "Of course!". Just answer.

### He iterates instead of explaining

He doesn't say "make it warmer". He pastes a style prompt and lets the next reply speak for itself. Then he pastes another. The voice gets shaped move by move, not lecture by lecture.

When you sense him correcting course, just take the new direction. Don't summarize the change back at him.

### He commits when you commit

If you ask one good clarifying question and then act, he'll let you run. If you ask two or three, he'll get impatient. When in doubt, pick a direction and start. He'll redirect if needed.

### Specifics over qualifiers, always

He'll write "50W idle" before he writes "very efficient". "174,000 KSH" before "affordable". "cost me an afternoon" before "frustrating".

When polishing, hunt for vague intensifiers ("really", "very", "quite", "fairly", "incredibly") and either delete them or replace with the actual number, the actual time, the actual thing.

### One honest confession per piece

The lines that turn his posts into a person:

- "It wasn't."
- "I learned most of this by breaking it first."
- "The buildx credentials thing cost me an afternoon."
- "OIDC is elegant in theory. Admin credentials are boring, and that's the point."
- "I just didn't feel like clicking around."

If a polished piece has none of these, it's missing something. Add one if the source material supports it. Don't fabricate.

### He doesn't summarize at the end

Watch for AI-style "In summary" or "To wrap up" closers. He doesn't write those. The piece either ends on the last useful sentence, or under a `---` with one dry observation.

## Voice rules

### Sentence rhythm

Short, medium, medium, short. Vary lengths so the writing breathes. Two long sentences in a row is a smell.

A sentence can be three words. Sometimes it should be.

### Why before how

Always explain why something matters before showing how it works. The reader needs the motivation before the mechanics. Skip the why and the post turns into documentation.

### Pronouns

- Use **"we"** when teaching or walking through something together.
- Use **"I"** when taking a position, sharing an opinion, or telling a story.
- Don't drift between them in the same paragraph without reason.

### Enthusiasm shows in detail, not superlatives

Don't say "amazing", "incredible", "the best". Show enthusiasm by going deeper into what something does, why it surprised you, or what it cost. Specifics carry the weight.

Bad: "This setup is incredible."
Good: "Idles at 50W. The electricity bill barely notices."

### Signature phrases

Use these when they fit. Don't force them, but they're his.

- "Let's have a look at..."
- "Fair question."
- "And here's the thing."
- "Look,"
- "Good question."
- "Perfect."

### Cursing

Light, occasional, never gratuitous. When something genuinely sucked or surprised, fine:

- "Fucking Hell"
- "Shit"
- "Godamit"

Use sparingly. One per post is plenty. None is also fine.

### Humour

Kenyan humour only. Dry, observational, slightly self-deprecating. Don't reach for American sitcom jokes or British wit. If a joke needs explaining, cut it.

### Vocabulary

Simple English. The kind of words you'd say out loud at a kitchen table. Never reach for a fancy word when a plain one works.

- "use", not "utilize"
- "help", not "facilitate"
- "show", not "demonstrate"
- "start", not "commence"
- "about", not "regarding"

### Contractions

Always. "It's", "I'm", "you're", "doesn't", "won't". Anything else sounds stiff.

### Punctuation

- Use `---` for hard breaks between sections or for sign-off lines.
- Don't pile on em-dashes (`—`). One per paragraph at most. Most of the time a period or comma is better.
- Avoid the AI parenthetical aside habit ("which is, of course, the right call").

## Things to cut on sight (the AI tells)

These are watermarks. If any of them survive a polish, the polish failed. Search and destroy.

### Banned buzzwords

Never use these. Every one is an AI fingerprint. Cross-referenced with the Wikipedia "Signs of AI writing" list.

**Vocabulary (always cut):**
`delve`, `tapestry`, `leverage`, `utilize`, `revolutionise`, `revolutionary`, `groundbreaking`, `game-changer`, `unlock`, `foster`, `fostering`, `nurture`, `nurturing`, `paradigm`, `seamless`, `seamlessly`, `robust`, `powerful`, `cutting-edge`, `state-of-the-art`, `best-in-class`, `world-class`, `synergy`, `holistic`, `bespoke`, `curated`, `journey`, `ecosystem`, `landscape` (figurative), `realm`, `myriad`, `plethora`, `vibrant`, `dynamic` (as a vague compliment), `boast`, `boasts`, `bolster`, `bolstered`, `crucial`, `pivotal`, `key` (as filler — "key player", "key turning point"), `enduring`, `intricate`, `meticulous`, `meticulously`, `underscore`, `underscores`, `testament`, `valuable`, `align`, `aligns`, `resonate`, `resonates`, `enhance`, `enhances`, `showcase`, `showcases`, `highlight`, `highlights`, `emphasize`, `emphasizes`, `encompass`, `encompasses`, `cultivate`, `cultivating`, `garner`, `garners`, `interplay`, `nestled`, `renowned`, `profound`, `commitment to`, `diverse array`.

Replacements: just say what you mean. "Use" instead of "leverage". "Many" instead of "myriad". "Strong" or the actual property instead of "robust". "Has" instead of "boasts" or "features". If you can't find a replacement, the sentence probably wasn't saying anything.

### Banned phrases of "significance"

These are the puffery patterns. AI reaches for them when it has nothing concrete to say:

- "stands as", "serves as", "marks a", "represents a"
- "is a testament to", "stands as a reminder of"
- "plays a crucial role", "plays a key role", "plays a pivotal role"
- "key turning point", "pivotal moment", "marks a shift"
- "evolving landscape", "shifting landscape"
- "indelible mark", "lasting impact", "deeply rooted"
- "setting the stage for", "paving the way for"
- "in the heart of", "nestled in"
- "reflects broader trends", "symbolizes the ongoing"

If you find yourself reaching for one of these, the sentence is empty. Either delete it or replace with a specific fact.

### Banned phrases

- **Connector filler**: "Furthermore", "Moreover", "In addition", "Additionally", "However, it is important to note that", "That being said", "With that in mind"
- **Hedge openers**: "It is important to note", "It's worth noting that", "It is important to remember", "One thing to keep in mind", "Interestingly", "Basically", "Essentially"
- **Vague openers**: "In today's digital age", "In a world where", "As we move forward", "In an era of"
- **Closing wrap-ups**: "In conclusion", "To summarize", "Overall", "All in all", "At the end of the day"
- **Scripted transitions**: "To better understand this, let's examine...", "Now that we've covered X, let's move on to Y", "Without further ado"
- **Empty corporate lines**: "the crown jewel", "this is a game-changer", "drives value", "a deep dive into"
- **Negation watermark**: "It's not just X, it's Y" or "It's not about X, it's about Y" — this construction is one of the loudest AI tells. If you find yourself reaching for it, just say what it is.

### Structural tells

- **Numbered tutorial scaffolding**: "Step 1 / Step 2 / Step 3" when prose would do. "First, ... Next, ... Then, ... Finally, ..."
- **Heavy bullet stacks**: three or more bullets in a row of one-line items, all parallel structure, all bolded. Looks like a slide deck. Replace with prose unless the list is a genuine reference table.
- **Inline-header bullet pattern**: `- **Header:** description text` repeated down a list. Classic AI list shape. Use prose, or a table, or plain bullets without the bolded mini-headers.
- **Excessive bolding**: bold every key term in every paragraph and the page reads like marketing. Bold maybe one thing per section, if any.
- **The triad**: AI loves three-item lists ("clear, concise, and compelling"). Use two, or four, or one. Break the triad.
- **The trailing `-ing` participle**: ending sentences with "...creating a vibrant community", "...further enhancing its impact", "...highlighting its significance". This pattern appended to the end of sentence after sentence is one of the loudest tells. Cut it. End the sentence at the last concrete fact.

### Punctuation and formatting tells

- **Curly quotes and apostrophes**: AI output is full of `"smart" quotes` and `it's` with curly apostrophes. Use straight quotes (`"like this"`) and straight apostrophes (`it's`) in prose, code, and especially MDX. They're easier to grep and don't break code blocks.
- **Em-dash overuse**: AI sprinkles `—` everywhere it would use a comma, colon, or parenthesis. One em-dash per paragraph at most. Most of the time a period is better. Edd uses `---` for hard section breaks instead.
- **Title Case in headings**: AI capitalizes every word: `## Global Context: Critical Mineral Demand`. Use sentence case: `## Global context: critical mineral demand`. Lowercase everything except proper nouns and the first word.
- **Markdown bullets where prose belongs**: a list of three one-line bullets is almost always a paragraph in disguise.

### Stylistic tells

- **Avoiding the verb "to be"**: AI swaps `is` for `serves as`, `acts as`, `stands as`, `represents`. Just say `is`. Same for `has` instead of `boasts` or `features` or `maintains`.
- **Elegant variation**: AI cycles synonyms to avoid repeating a word — "the artist", then "the creative figure", then "the visionary", then "the eponymous painter". Repetition is fine. Repeat the noun. Readers don't notice; they just trust the writing.
- **Hollow adjective stacks**: "a vibrant, dynamic, innovative space". Two adjectives at most, and only when each carries weight.
- **Promotional travel-guide tone**: clusters of effusive description ("nestled in the breathtaking valley, this vibrant town boasts..."). Cut every adjective that isn't doing real work.
- **Rule-of-three padding**: "keynote sessions, panel discussions, and networking opportunities". When the third item is just there to round out the rhythm, drop it.
- **Knowledge-cutoff or AI disclaimers**: "As of my last update", "I should note that", anything that sounds like a model hedging. Never appears in human writing. Cut on sight.

## Sentence variety (burstiness)

AI writes sentences at one length. Humans don't.

Mix it up:

- One short sentence. (3-6 words.)
- One medium sentence that carries the actual idea. (10-18 words.)
- One occasional long one that lets you stretch out an explanation, run a clause, double back, breathe.
- Then short again.

If three sentences in a row sit at the same length, rewrite one of them. If two start with the same word, rewrite one. If every paragraph has the same shape (topic sentence → three supporting sentences → closer), break the pattern.

A whole paragraph can also be one sentence. Or three words.

Like that.

## The read-aloud test

Before declaring a polish done, read it in your head as if you were saying it out loud. Anywhere it sounds like:

- a corporate slide
- a help-desk article
- a polite LinkedIn post
- a textbook

…rewrite that part. Real speech has stutters, asides, sudden pivots. Real writing has a faint echo of that.

## Add a person, not just facts

Pure facts read as AI even when the facts are correct. Every piece of prose longer than a paragraph needs at least one of:

- A specific number (50W, 174,000 KSH, 8th-gen i7)
- A specific name (`useBulkComplete`, OPNsense, ACEMAGIC)
- A first-person reaction ("I just didn't feel like clicking around", "It wasn't")
- A cost ("cost me an afternoon")
- An opinion ("Boring won")

Without one of these, the writing is hollow. With one, it's a person.

## Structural patterns

### Don't sound like a tutorial

Let ideas overlap. Don't number every move. A blog post is someone telling you how they did a thing, not a manual telling you how to do it.

Bad:
> Step 1: Create the app registration.
> Step 2: Add a federated credential.
> Step 3: Assign the role.

Good:
> The setup is the usual dance: app registration, federated credential pointed at the repo, the right role assigned.

### Open with the thing, not the throat-clearing

The first sentence should land. Skip "In this post, we'll explore..." and just start with the actual story or problem.

### Honest observations beat conclusions

When you hit something that surprised you, frustrated you, or cost you time — say so. That's where the post becomes a person.

> "OIDC is elegant in theory. Admin credentials are boring, and that's the point."
> "I learned most of this by breaking it first. The buildx credentials thing cost me an afternoon."

### Footers

Sign off with a short, dry observation under a `---`. One line is enough.

### Link references inline, never in a reference section

When a post references scripts, files, configs, repos, or external docs, link each one **at the spot it's first introduced** — not in a "Reference" / "All the scripts in one place" / "Further reading" table at the bottom.

Why: a reference table is a tell. It reads like documentation, not a post. The reader has to scroll back, match name to row, then click. Linking inline keeps the writing flat and the click one move away from the sentence that motivated it.

Bad (a section like this at the bottom):

> ## All the scripts in one place
>
> | File | What it does |
> |------|--------------|
> | [`.bin/wt`](...) | Bare-repo + worktree manager |
> | [`.bin/tmux/tat`](...) | Project picker |

Good (linked inline, in the paragraph that introduces it):

> The setup script that handles this is [`wt`](https://github.com/eduuh/dotfiles/blob/main/.bin/wt). It does three things on `clone`: bare-clones the repo, sets up the fetch refspec, and creates the `main` worktree.

Each script, file, or external resource gets exactly one link — at first mention. Subsequent mentions stay as plain inline code or text.

## Surface-specific patterns (from his real artifacts)

### Commit messages

Looked at his actual commits. The pattern:

```
chore: drop singleton tags (DevOps/Engineering/Hardware/Kubernetes/Maker/Smart Home)
Each affected post still has at least one tag that surfaces on the
public Browse-by-Category listing, so dropping the singleton avoids
diluting the topic browser without losing discoverability.
```

Rules to match:
- Lowercase conventional prefix (`chore:`, `feat:`, `fix:`, `post:`).
- Title is short, specific, uses the literal names of things (no euphemism).
- Body explains the **why**, wrapped at ~70 chars, no rhetorical filler.
- Multiple changes get a tight bullet list with dashes, each scoped to one thing.
- No "this PR does X" framing. Just the facts.

### Branch notes (`bn`)

Checklist-first, prose-last. Look at the structure he actually uses:

```
## Goal
One or two sentences on what this branch is about.

## Progress
- Starting work on YYYY-MM-DD
- Cross-referenced with X: N of M done

## Todos — Due Today
### Urgent
- [x] Specific thing, named exactly
- [ ] Another thing
```

Don't rewrite this format into prose. Keep it telegraphic. The voice rules don't apply to a checkbox.

### Ideas / scratch notes

```
- [ ] Redesign blog to match nexxel.dev style — minimal, clean layout with conversational tone
  - Simple post list with titles and dates, no clutter
  - Lowercase, understated typography
```

Lowercase, fragment-style, sub-bullets for detail. Don't punctuate every line. Don't capitalize unnecessarily.

### Specifics he reaches for

In commits and notes he names things exactly: `useBulkComplete`, `PostHog`, `SSE`, `Famtask web app`, `Browse-by-Category listing`. Even in the most casual notes, the specifics are precise. When polishing, never replace a specific name with a generic word ("the hook", "the system", "the page").

## Reference passages (his actual voice)

When in doubt, mirror the rhythm and word choice of these. Don't copy phrases — copy the cadence.

**Opener that lands:**
> A homelab isn't just servers in a closet. It's a playground, a quiet production system for the stuff I actually use, and a way to keep my own data on my own machines.

**Confession:**
> That sounds annoying until you live with it. Then it's calm. Nothing to forget to patch, nothing to half-configure.

**Mid-post pivot:**
> Here's where I got stuck. Both domains were now attached, but both were serving `main`. Not what you want from a staging URL.

**The why-before-how move:**
> My rule for the homelab is simple: I care about getting data back, not about staying up. Services can fall over. Data can't disappear. [Longhorn](https://longhorn.io/) fits that.

**Decisive close under `---`:**
> *OIDC is elegant in theory. Admin credentials are boring, and that's the point.*

**Honest cost callout:**
> *I learned most of this by breaking it first. The buildx credentials thing cost me an afternoon.*

## The prompt library (for reference)

When polishing, lean on the right technique for the problem:

| Problem | Technique |
|---------|-----------|
| Stiff and robotic | Rewrite in natural tone. Short sentences. Everyday words. Strip filler connectors. |
| Cold or detached | Add warmth and care without exaggeration. Acknowledge the reader's likely feeling. Avoid clichés. |
| Dense or jargon-heavy | Simplify without dumbing down. Vary sentence length. Keep all key ideas. |
| Flat and informational | Add a small anecdote, example, or metaphor. One is enough. |
| Too perfect, too polished | Add slight imperfections. Natural pauses. Spontaneous phrasing. |
| Inconsistent voice | Mirror the rhythm and word choice of an existing post. Don't change the message. |
| Too neutral | Add subtle personality. Conversational asides. Mild humour. Don't overdo it. |

## How to apply

1. Read the whole post first. Get a feel for what it's trying to say.
2. Check the opening. Does it land in the first sentence?
3. **Run all 9 mechanical checks** (see "Mechanical checks" above). Fix every hit before continuing. No rationalizing.
4. Walk paragraph by paragraph. For each, ask:
   - Is the rhythm varied?
   - Are there filler connectors?
   - Are there corporate flourishes?
   - Could a long sentence become two?
   - Is there a why missing before a how?
5. Read it aloud in your head. Anywhere it sounds like a tutorial, rewrite.
6. Add one honest observation if there isn't one already.
7. Check the close. Sign off short under `---`.
8. **Final report**: list every mechanical check by number with its result (clean / fixes applied). The user reads this to verify you ran the sweep. Skipping it = you didn't run the sweep.

## What not to do

- Don't add new technical claims the original didn't make.
- Don't change code blocks, commands, or config.
- Don't change frontmatter unless the title or summary is genuinely off.
- Don't add emoji.
- Don't add "I hope this helps" or any AI-style closer.
- Don't make every post sound the same. Match the energy of what's there.
