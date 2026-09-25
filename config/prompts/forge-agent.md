---
name: forge-agent
description: Run bn's task list from Forge — assign a task to Agents, and bn claims it, works it through the pipeline, comments on progress and completes it when the PR merges
when: Someone wants bn to pick up work assigned in Forge, is switching the task provider to Forge, or is asking why an Agents task was not claimed, commented on or completed.
keywords: [forge, wira360, agents, claim, task provider, forge login, assign to agents, pr merged]
---
Help a person run bn's Forge loop, or find out why it is stuck. Explain and check; do not change
Forge data by hand unless they ask.

## The flow

1. **Assign.** In Forge, a task under a **Projects** goal whose Repository is set (for example
   `https://github.com/eduuh/bn`) is assigned to the **Agents** member.
2. **Claim.** On a machine with `[tasks] provider = "forge"`, `bn forge sweep` lists open agent
   tasks. Nothing runs it for you: it runs when a person runs it, or from their own cron, and it
   does its work at most once a minute however often it is run. It claims a task when: nothing has
   started it, its repository resolves to a clone on this machine, `[machine] repos` allows that
   repo, and this machine holds fewer than `--max` (default 1) claimed tasks that have no PR yet.
   Forge's `claim_activity` is atomic, so two machines never both win; the loser moves on silently.
   The winner writes `repo:<local clone name>` and comments `claimed by bn@<node>`.
3. **Start.** The sweep starts each claim with `bn dispatch --task <id>` in that clone, which binds
   the task's `branch:` and launches the agent. From there it is an ordinary wip task: a SMART gate
   refusal comments `needs more detail: ...` on the task, and every stage change comments
   `stage N/9: <stage> (<repo>:<branch>)`.
4. **Done.** `bn agent done --summary "..." --pr <url>` comments the summary and PR link and stores
   `pr:` on the task. A task with a PR stays open until the merge; without one it completes now.
5. **Merge.** The next `bn forge sweep` asks `gh pr view <pr> --json state` about each such task, at
   most every ten minutes. `MERGED` completes the task in Forge with `PR merged, completing`. `CLOSED` comments
   once and leaves the task for a person.

Every comment is best-effort: when Forge cannot be reached the pipeline step still happens and the
failure is printed as `bn: could not leave a note on task ...` on the step's stderr.

## Switching the provider

In `<notes_root>/config.toml`, or the machine's own `<notes_root>/<machine>/config.toml`:

```toml
[tasks]
provider = "forge"                          # "file" (tasks.txt) is the default
forge_url = "https://api.wira360.com/mcp"   # the default; point at a local Forge to test
```

There is no fallback: with `provider = "forge"` and no key, every task command fails and says so.
Setting `provider = "file"` goes back to `tasks.txt`, untouched by Forge mode.

## The key

- `bn forge login` opens Forge in a browser (needs `@playwright/cli`), waits for sign-in, mints the
  Agents key and saves it to `<config_dir>/forge-token` (mode 0600).
- `bn forge login --paste` reads a `fam_...` key from stdin instead.
- `BN_FORGE_TOKEN` wins over the file.
- `bn forge status` shows where the key comes from, whether the MCP session opens, the Agents
  member, the Projects goals with their repos, and open and claimed agent task counts. Run it first.

## Troubleshooting

- **Never claimed.** Check in order: `bn forge status` lists the goal with a repo and an Agents
  member; the task is assigned to Agents and not started (a start date blocks claiming); the repo is
  cloned here (`bn wt clone <url>`, the clone may have a different local name, which is fine); the
  machine's `[machine] repos` allows it; someone ran `bn forge sweep` on this machine; this machine
  does not already hold `--max` claims without a PR (`bn forge status` counts them). The sweep does
  its work at most once a minute (`<state_dir>/forge-sweep.json`); `bn forge sweep --json` shows
  what each step did.
- **Claimed but not moving.** `bn forge sweep --json` says whether the claim started and, if not,
  why. A claim that did not start is not retried, because it is no longer unstarted: start it with
  `bn dispatch --repo <repo> --task <id> "<title>"`. `bn task show <id>` shows its `smart:` stamp.
- **No comments.** Look for `could not leave a note` in the output of the command that moved the
  task. A rejected key names `bn forge login`.
- **Not completing after merge.** `pr:` must be a full PR URL, `gh auth status` must pass on the
  machine, and the task's `claimer:` must be this machine (or empty). Wait out the ten-minute poll.
- **Id jumps.** bn never reuses a task id; the highest one per endpoint is kept in
  `<state_dir>/forge-ids.json`, refreshed from a year of Forge history when it is a week old.

## Trying it safely

Never experiment against production Forge or with the machine's real config. Point `forge_url` at a local
Forge, sandbox bn with `BN_HOME` (plus `BN_WORKTREE_DIR` if anything may dispatch), and export
`BN_NO_AUTO_TICK=1 BN_NO_CATCHUP=1` before running a dev binary.
