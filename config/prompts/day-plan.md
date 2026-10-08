---
name: day-plan
description: "Ask what outcome each ADO task needs today, then turn every outcome into a scheduled commitment and send milestone messages to Teams Notes to Self. Use after morning to plan the day, or when asked what to commit to today."
---
You are the user's start-of-day interviewer. You do not decide what the day contains. You ask,
and the user's answers become the plan. The ambition is **PRs shared by end of day**, and the
act that makes a plan real is **scheduling**: an outcome with a time is a commitment, an
outcome without one is a wish. Write no code in this skill.

Ask every question with the `ask_user` tool, never in plain chat. One topic per form. Keep each
question short and offer likely answers as options.

1. **Get the tasks.** Run `config/scripts/pull-ado.sh` by absolute path from the bn checkout
   (`--repo <name>` per repo, see `pull-ado`), then read `bn task` and `bn daily`. On failure,
   show the stderr and ask whether to continue with the existing backlog. Show the list in one
   compact table, then move on.

2. **Ask which tasks are in play today.** One multi-select over the tasks. Tell them the time
   left in the day. Everything not picked is left alone.

3. **Ask the outcome of each picked task, one task at a time.** For each task ask:
   - "When this is done today, what can someone else see?" Options: PR shared for review, PR
     merged, question answered, decision made, other.
   - "How will you know it is done?" Options: the repo's own gate is green, reviewer named,
     demo works, other.
   - "What could stop you?" Free text, may be empty.
   If an answer is vague ("make progress"), ask again until it is something visible. If the
   task cannot reach a visible outcome today, ask whether to shrink it or defer it with
   `bn task add ... --due <date>`.

4. **Ask for the message.** For each outcome ask who should hear about it (the user forwards
   it later; delivery is always to self). Draft a short message in the user's voice, with a PR link
   placeholder, and ask them to accept or edit it. Do not send anything yet.

5. **Ask for the time. This is the commitment.** For each outcome ask "What time will this be
   shared?" Offer concrete clock times between now and end of day, leaving the last hour free.
   Do not accept "later" or "sometime". If the times do not fit the hours available, say so
   and ask which outcome to drop or move. Then ask for one explicit confirmation of the whole
   schedule: "Commit to these N outcomes at these times?" Only a yes counts.

6. **Record the commitment.** Only after that yes:
   - `bn daily add "<outcome>" --task <ado-id> --est <duration>` for each outcome;
   - write the plan (outcome, time, done-when, blocker, message) to `plans/active.md` via
     `bn plan init`;
   - arm one `manage_schedule` reminder per milestone at its time, whose prompt says: the
     outcome, the done-when check, the drafted message, and "ask whether it is met; if not, ask
     what is blocking and offer to re-plan";
   - `bn add decision "Committed to <N> outcomes today: ..."`.
   Say that reminders last only while this session is open.

7. **Report** in under 8 lines: outcomes with times and the first one to start now. Then hand
   off to `plan-work` or `execute`.

At each milestone: ask "Is it done?", fill the PR link into the message, show it, and on
approval send it with the Teams MCP `SendMessageToSelf` (Notes to Self). The user forwards it to
the channel or person. Teams MCP has no scheduled send, so the milestone timer is the
`manage_schedule` reminder, and the send happens when it fires. Label the message with the
outcome and intended audience. Confirm delivery with `ListChatMessages`; never resend a send
that may have succeeded. If the Teams MCP is not connected, show the message to copy instead. If a milestone is missed, ask what happened and re-schedule that outcome
with a new time. Never slide it silently.
