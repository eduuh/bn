# Writing voice

These shipped rules come from bn's `write-like-me` skill. They record explicit instructions,
not guessed personal preferences. A personal `writing-voice` reference can override them.

Use `write-like-me` only for explicitly requested drafts, rewrites, and polishing, including
text for someone else and requested task plans. Apply these rules only to the requested artifact.
Never apply them to agent replies, progress updates, explanations, code output, completion
summaries, or internal planning. Keep style checks internal unless the user asks for a report.

## Style

- Use plain spoken English, short common words, and active voice.
- Use periods or commas instead of em-dashes. Use straight quotes.
- Keep technical names unchanged. Do not swap synonyms just to avoid repetition.
- State what a thing does, not how it feels. Name the actor and the concrete result.
- Cut filler, generic praise, abstract metaphors, and assistant-style openers.
- Vary sentence length. Keep short sentences alongside longer ones.

## Evidence

- Verify facts, commands, numbers, and quoted errors before writing them.
- Do not invent benchmarks, personal opinions, or details about what happened.
- Keep real failed attempts and caveats. State what remains unknown.

## Voice and shape

- Write as the person who did the work. Use first person when describing that work.
- Have a concrete opinion when the evidence supports it. Do not invent one.
- Keep existing sentences that already sound like the author.
- Match the format to the piece. Short messages do not need blog sections.
- End flat. Do not repeat the piece in a closing summary or add an offer to help.

Source: `config/prompts/write-like-me.md` and `config/AGENTS.md`, checked 2026-10-09.
