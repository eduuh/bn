---
name: multi-device
description: Work across several machines (laptop, Cloud PCs, other PCs) as one - put a machine on the bn tunnel mesh, see which machines are up and why not, run an agent on another machine that survives dropped connections, message agents wherever they run, and upgrade every machine.
when: A task involves another machine - adding or fixing a machine (a VPS or any box you can ssh to, via bn machine join), a machine missing from bn machine tunnels, launching or steering an agent on a Cloud PC, running a long command remotely, or upgrading bn on several machines.
keywords: [multi-device, machines, remote, cloud-pc, tunnel, tunnels, mesh, ssh, host, launch, attach, durable, upgrade, lan]
---

# Working across machines

Every machine runs bn and joins one mesh of Dev Tunnels. Each machine is reachable as `ssh <machine>`, where the alias is its short hostname (`hostname -s`), and bn keeps those connections up by itself. Treat the other machines as more seats of the same desk. Nothing here needs a VPN or an open port.

What makes the mesh:

- **One Dev Tunnels account on every machine.** A machine signed in with a different account is invisible to the rest. `devtunnel user show` must say the same account everywhere.
- **Hosting** means a key-only, user-level sshd behind the machine's tunnel. It needs the `sshd` binary. A machine without it can reach the others but cannot be reached.
- **The watchdog** (`tunnel-auto`, every minute in tmux and every 5 minutes from the cron) hosts, connects, repairs, and publishes this machine's status report.

## Put a machine on the mesh

Run on that machine, at a terminal:

```sh
bn run tunnel-setup
```

It installs what is missing (sshd, tmux, nc, curl, ss, gh), downloads the devtunnel CLI, runs the sign-in, turns on linger, the timers and the WSL keeper, then hosts and connects. Every step prints `ok`, `done` or `NEEDS YOU` with the exact command, so running it again is also how you check a machine. Options:

- `BN_TUNNEL_HOST=0` makes a connect-only machine: it never hosts, sshd is not installed, and the others drop it.
- `BN_TUNNEL_LAN=1` turns on local network mode. On the same LAN, ssh goes straight to the machine and uses the tunnel only as a fallback. On WSL this needs mirrored networking and one Windows firewall rule, and setup prints both.

- `BN_TUNNEL_NICK=<name>` gives the machine a nickname (letters, digits, dashes). The hostname stays its real name (`hostname` shows it). The nickname leads the terminal title and tmux status bar, shows in `bn machine tunnels`, and is an ssh alias on the other machines (in lowercase). It is saved on the machine (`~/.config/bn/tunnel.conf`) and recorded in the notes repo (`bn/tunnel.conf`, one `[machine "<hostname>"]` section each), so a rebuilt machine gets it back.

A signed-in machine enrols itself within 5 minutes even without setup. Opt out with `git config --global bn.tunnelAuto off`.

## Join a machine over plain ssh (a VPS, a LAN box)

No Dev Tunnels account is needed when this machine can already `ssh` to the other one with a key:

```sh
bn machine join <name> <user@host[:port]>     # e.g. bn machine join contabo root@161.97.172.122
```

Run it once, on the machine you are on (usually the laptop). It writes a `# >>> bn mesh <name> >>>` block into `~/.ssh/config`, trusts the other machine's key here, and holds a reverse forward (`ssh -R`, tmux window `bn-tunnels:mesh-<name>`) from that machine's loopback to this machine's sshd. It also writes a block for this machine into the other one's `~/.ssh/config`, so `ssh <this-machine>` works from there even though this machine is behind NAT. It ends by proving both directions.

- This machine needs an sshd for the way back: macOS Remote Login on, or bn's user-level sshd (started on demand on Debian/Ubuntu).
- `--back user@host[:port]` when this machine is reachable directly (no reverse forward); `--no-back` for one way only.
- `bn machine mesh` shows each node: ssh each way, its bn version, its live agents. `bn machine leave <name>` undoes the join on both sides.
- The watchdog (`tunnel-auto`) keeps mesh nodes too: a dead forward window, a stale shared connection after sleep, or a port held by a dead session on the node is repaired within a pass. Mesh nodes show in the tmux `⇄` segment.
- A machine on the mesh both ways is just another tunnel host to `bn agent msg`, `bn agent live --mesh` and `bn agent launch --host`.

## See the mesh

```sh
bn machine tunnels
```

This lists every host on the account, including ones this machine has not connected to yet. For each it shows the route (`tunnel :<port>` or `LAN <ip>`), whether anything is hosting its tunnel (`HOST-END`), and a reason when it is down. The reason is the machine's own report when there is one ("devtunnel is signed out", "silent since 20:10"). Reports travel as `refs/heads/bn-status/<machine>` in the work-notes repo, so they still say something when the machine itself is off. It takes about 10 seconds. The `⇄` segment in the tmux status bar shows only connections whose latest watchdog check passed. Disconnected hosts stay in `bn machine tunnels`, not the bar; `⇄ ?` means the watchdog cache is stale.

## Run an agent on another machine

From the worktree of the branch, on this machine:

```sh
bn agent launch claude --host <machine>
```

The agent starts in a tmux window on that machine (session `<repo>`, window named after the branch). If the branch has no checkout there yet, `bn wt add` makes one, which needs the branch pushed. If the branch's window is an idle shell, the agent starts in that shell, so you are back at the prompt when it exits. This terminal only attaches:

- A dropped connection, a sleeping laptop, or a closed terminal does not stop the agent. The view attaches again by itself while the window exists.
- Detach with the tmux prefix and `d`. Running the same command again attaches to the running agent. It never starts a second one.
- The first run of a harness on a new machine may stop at its login or onboarding screen. Finish it in the attached view.

## Edit files on another machine

```sh
bn edit remote <machine>
```

From the branch's worktree here. nvim runs on that machine, in a tmux window there, with that checkout's files, LSP and git. This terminal is its UI. A dropped connection reconnects to the same editor, with the same buffers and undo. `:detach` leaves it running (run the command again to come back), and `:qa` quits it. If that worktree already has a `bn edit` nvim there, you share it.

## See a plan an agent made on another machine

```sh
bn plan view --host <machine>
```

This copies the newest `*.html` from that branch's note folder on the machine and opens it in the browser here (the Windows browser from WSL). No port forwarding is needed. For a live page instead (a Lavish session, a dev server on that machine), add `--port <n>`: it forwards the port, opens `http://localhost:<n>`, and forwards again after a drop until Ctrl-C. Without `--host` it opens the newest plan HTML of this branch here. A remote agent should save plan HTML under `$(bn --path)/plans/`, so this finds it.

## Talk to agents on other machines

```sh
bn agent msg <repo:branch> "..."
```

This works wherever the target runs. When the target is not live here, bn asks every mesh machine (tunnel hosts and `bn machine join` nodes) for its live agents and hands the message to the one that has it. A remote agent replies the same way, so an agent on a Cloud PC or a VPS can answer the coordinator on the laptop. A target live on two machines is refused, not guessed. Replies need the laptop to be reachable back: host a tunnel, or join the node with `bn machine join` (its reverse forward is the way back). `bn agent live --mesh` lists every agent on every machine, which is who `bn agent msg` can reach; a machine that "did not answer" is down or has no bn.

## Run commands on another machine

- A quick command: `ssh <machine> '<command>'`. It rides one shared connection per machine, so repeated calls cost well under a second.
- Anything longer than a minute (builds, installs, clones) goes in a tmux window there, never on a bare ssh session. A tunnel blip kills the session and everything under it. Write the job as a script file first, so there is one layer of quoting instead of three (nested `bash -lc "..."` inside a tmux command inside ssh broke silently twice: the window closed at once and nothing ran):

  ```sh
  ssh <machine> 'cat > /tmp/job.sh <<"EOF"
  cd ~/projects/worktree/bn/main && git merge --ff-only origin/main && ./build.sh
  echo "job-rc=$?"
  EOF
  tmux new-window -d -t "=bn-tunnels:" -n job "bash -l /tmp/job.sh > /tmp/job.log 2>&1"'
  ```

  Then wait on the exact marker the script writes (`grep -q "^job-rc=" /tmp/job.log`), in one background loop for all machines. A marker that does not match what the script prints, or a leftover log from an earlier run, makes the loop wait forever or end at once: delete the log before starting, and stop a waiter you replace.
- A remote non-login shell has neither bn's install dirs nor nvm on its PATH (nor Homebrew on a Mac). Start with `export PATH=$HOME/.bn/bin:$HOME/.bin:$HOME/.local/bin:/opt/homebrew/bin:$PATH`, or use `bash -lc` with `source ~/.nvm/nvm.sh`.
- The remote login shell may be zsh, which expands a bare `=word` to a command path (`tmux list-windows -t =bn` fails with "bn not found"). Quote targets as `'=bn'`, or run the command under `sh -c`. zsh also does not split an unquoted `$var` into words, reads `$var:r` as a modifier (write `${var}:...`), and treats `(x86)` in an unquoted word as a glob.
- PowerShell from WSL: a multi-line script breaks under `powershell.exe -Command -`; send it with `-EncodedCommand` (UTF-16LE, base64). Files in Windows repos are CRLF: edit bytes and keep the `\r\n`.
- In this agent's own shell, `git` may be a wrapper function that fails on `/mnt/c` paths: use `command git` there.

## Work efficiently across machines

Lessons from a long session that set up four machines; each cost real time before it was learned.

- **Look before acting.** `bn machine tunnels` (about 10 seconds) says which machines are up, their names, and why one is down. Start there instead of probing each machine with ssh.
- **Fan out, then wait once.** Start the same job on every machine in its own tmux window, then wait in one background loop for every marker. Do not poll with repeated short commands.
- **Git on a machine without credentials: push to it, never pull on it.** `git push ssh://<machine><repo path> <ref>:refs/remotes/origin/<ref>`, then `git merge --ff-only origin/<ref>` there. The same works for the notes repo and the nvim config. A `git pull` there hangs on a username prompt.
- **bn's clones fetch only `main`.** For a branch: `git fetch origin <branch> && git merge --ff-only FETCH_HEAD`.
- **Pick the GitHub account for https pushes.** `gh` can hold two accounts; the active one is used. The notes repo (`work-account/branch-notes-work`) and the personal repos (`eduuh/*`) need different ones: `git -c credential.helper= -c 'credential.helper=!gh auth git-credential' push https://github.com/<owner>/<repo>.git <branch>` with the right account active, or push over ssh.
- **A notes push that GitHub rejects (GH013)** has a secret in an unpushed commit, usually a tool log saved by an automatic checkpoint (`.playwright-cli/` console logs held an Entra token). The notes `.gitignore` covers that folder now; if it happens again, `git reset --soft origin/main`, `git rm --cached` the file, commit once, push.
- **Settings for one machine never go in `~/.gitconfig`.** It is synced by dotfiles to every machine. bn's tunnel settings live in `~/.config/bn/tunnel.conf`.
- **Check the bn binary against the config.** A machine with an old `bn` and a new `bn/main` prints TOML errors and runs unknown jobs as queued tasks. The published release lags `main`; build with `./build.sh`. `bn --version` must match `git -C ~/projects/worktree/bn/main log -1 --format=%h`.
- **Test destructive recovery on a machine that can take it**, and say what it will kill first (a WSL shutdown kills running builds there).
- **Socket paths stay under 108 bytes.** An ssh `ControlPath` or nvim socket in a deep scratch directory fails with "path too long".

## Upgrade bn everywhere

On each machine, in its `bn/main` worktree, inside a tmux window (see above):

```sh
git pull --ff-only && ./build.sh && bn run tunnel-setup
```

A machine with no GitHub credentials asks for a username on `git pull` and hangs there. Push to it from a machine that has the commits instead, then fast-forward it:

```sh
git push ssh://<machine>/home/<user>/projects/bare/bn.git origin/main:refs/remotes/origin/main
ssh <machine> 'git -C ~/projects/worktree/bn/main merge --ff-only origin/main'
```

Check with `ssh <machine> 'export PATH=$HOME/.local/bin:$PATH; bn --version'`: every machine should show the same git hash.

To try an unmerged branch on another machine, remember that bn's bare clones fetch only `main`: `git pull` in that branch's worktree fetches nothing new. Use `git fetch origin <branch> && git merge --ff-only FETCH_HEAD`, then `./build.sh`.

## Keep machines up

- **WSL keeper** (`bn run wsl-keepalive`, which setup does): holds the distro open once you are signed in to Windows, and stops Windows sleeping on AC power. It fires at sign-in and every 5 minutes, so a WSL that stopped while you stayed signed in comes back within 5 minutes. Before, it only came back at your next sign-in.
- **Boot task** (`sh wsl-keepalive.sh boot`, one UAC prompt): starts the keeper at boot, before anyone signs in, so a host comes back after an overnight Windows Update. It did not start on an Entra-joined laptop signed in with a domain account. The script removes it again in that case, so test it on each machine.
- **Hosting a WSL machine** without the boot task: it stays down after a reboot until someone signs in.

## When something is wrong

| Symptom | Cause | Fix |
|---|---|---|
| A machine is missing from `bn machine tunnels` | It has no tunnel on this account: never set up, signed out, a different devtunnel account, or bn older than the first-run fix | On it: `devtunnel user show`, upgrade bn, `bn run tunnel-setup` |
| `DOWN`, `HOST-END no` | Nothing hosts its tunnel: the machine is off or asleep, WSL stopped, or devtunnel is signed out there | Its report line says which. Sign in to that machine, or run `wsl-keepalive boot` there |
| `Permission denied (publickey)` | This machine's key is not on the GitHub account the host trusts, or the host has not re-read the keys | `bn run tunnel-setup` here, then `bn run tunnel-host` there |
| `silent since <time>` | Its watchdog stopped reporting: off, asleep, or the watchdog died | Wake or sign in to it; check `bn machine tunnels` there |
| The agent view says "lost the connection" | The tunnel dropped. The agent keeps running | Wait; the watchdog rebuilds the tunnel within about a minute and the view attaches again |
| `bn agent launch --host` says "no checkout there, and none could be made" | The branch is not pushed, so `bn wt add` there could not check it out | `git push -u origin <branch>`, then launch again |
| `git pull` asks for a username on a remote | That machine has no GitHub credentials | Push to it over ssh (see "Upgrade bn everywhere") |
| `bn machine tunnels` says "report not published yet" on a machine | Its notes repo cannot push (no GitHub credentials there) | `gh auth login` and `gh auth setup-git` on that machine |
| A new nickname does not show on the other machines | They re-read nicknames on their 15-minute reconnect pass | `bn run tunnel-connect` on each of them |
| `devtunnel` dies with "Couldn't find a valid ICU package" | A .NET program, and the machine has no libicu | Current bn wraps it; or `sudo apt-get install -y libicu-dev` (dotfiles installs it) |
| `bn run tunnel-setup` prints TOML errors and "queued bn/run-tunnel-setup" | The installed `bn` is older than `bn/main` | `./build.sh` in `bn/main` (the published release is older still) |
| `Host key verification failed` between two machines | The first connection needs the host key recorded; old tunnel blocks did not do it unattended | `bn run tunnel-connect` there (writes `StrictHostKeyChecking accept-new`), or `ssh <machine> true` once by hand |

## Don't

- Don't run a long job over bare ssh. Use a tmux window on the remote.
- Don't kill `bn-tunnels` windows by hand to "fix" a tunnel. The watchdog restarts a dead forward itself, and `bn machine tunnels` says what is actually wrong.
- Don't pass a host name you did not get from `bn machine tunnels` or `~/.ssh/config`. bn refuses anything that could be read as an ssh option.
- Don't sign a machine in to Dev Tunnels with a second account "to test". It splits the mesh in two.
- Don't `pgrep -f`/`pkill -f` with a pattern your own command line also contains; it finds (or kills) the command doing the looking. Match on the process name and parent instead.
- Don't leave a background waiter running after you change the job it waits for; stop it, or it waits forever on a marker that will never appear.
