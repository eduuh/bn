---
name: dev-tunnel
description: Open a dev tunnel on request, and reach a local dev server from another machine or person. Invoke whenever asked to open/start a devtunnel, share a local port or running app, get a URL for a colleague, or test on a phone or another laptop — it opens the tunnel and reports the URL. Covers Microsoft Dev Tunnels for sharing outward, Tailscale for private machine-to-machine, and SSH forwarding as the zero-install fallback.
when: Asked to open/start a devtunnel, share a local port or running app, get a URL for a colleague, or test on a phone or another laptop — it opens the tunnel and reports the URL.
keywords: [dev-tunnel, dev, tunnel, open, request, reach, local, server, another, machine, person, invoke]
---
## When asked to open a tunnel, do this

Don't explain the options — open it. Only stop to ask if step 1 is genuinely
ambiguous.

1. **Settle exposure.** If they said "share with X" or named a person, that is a
   Dev Tunnel. If they said "my other machine"/"my phone on the same account",
   that is Tailscale. Default a Dev Tunnel to **authenticated** (no
   `--allow-anonymous`) unless they asked for a link anyone can open — and if they
   did, say so in your reply, once, plainly.

2. **Check the port is actually serving.** A tunnel over a dead port looks broken
   and wastes the round trip:

   ```bash
   ss -tln | grep -E ':(3000|3001)'
   ```

   Nothing listening → the app is the problem, not the tunnel. Say so and stop; see
   `onejs-build` for getting the BigMono dev server up.

3. **Check the tool exists**, and install it only if they want that:

   ```bash
   command -v devtunnel || echo "not installed"
   devtunnel user show 2>/dev/null || devtunnel user login   # login opens a browser
   ```

4. **Run it in its own named window** — it holds the foreground for as long as the
   tunnel lives, so it must not share a pane with anything else:

   ```bash
   tmux new-window -d -t "$(tmux display-message -p '#S')" -n devtunnel \
     'devtunnel host -p 3000 -p 3001 --protocol https; exec zsh'
   ```

5. **Report the URL**, read from that window's output — not guessed, not
   constructed. Wait for it to appear rather than replying before it exists:

   ```bash
   tmux capture-pane -p -t "<session>:devtunnel" -S -40 | grep -Eo 'https://[a-z0-9-]+\.devtunnels\.ms[^ ]*'
   ```

6. **Verify from outside** before calling it done — see the section below.

Tearing down is Ctrl-C in that window, or `tmux kill-window -t <session>:devtunnel`.
Do it when they say they're finished, and mention that a forgotten tunnel stays
open.

---

Three ways to reach a local dev server from elsewhere. They are not
interchangeable — pick by **who needs access**, not by which one you remember.

| you want | use | who can reach it |
|---|---|---|
| share a running app with a colleague, or open it on a phone | **Dev Tunnels** | anyone with the URL (or your tenant, if not anonymous) |
| your own other machines, permanently | **Tailscale** | only devices on your tailnet |
| one port, one session, a machine you already SSH to | **SSH forwarding** | just you |

Infer it from what they said and act; ask only when it is genuinely ambiguous.
"Share this" and "reach my desktop" have different answers, and the first one puts
a port on the public internet.

## Dev Tunnels (Microsoft) — sharing outward

This is what `start.sh` points at once the BigMono app is up on 3000/3001:

```bash
devtunnel host -p 3000 -p 3001 --protocol https --allow-anonymous
```

`--allow-anonymous` means **anyone with the URL can reach it, unauthenticated**.
Drop it to require a Microsoft sign-in from your tenant, which is the right default
for anything touching real data. Say plainly which one you used.

Not installed by default — check `command -v devtunnel` first. Install:

```bash
curl -sL https://aka.ms/DevTunnelCliInstall | bash
devtunnel user login          # opens a browser
```

Run it **beside** the dev server, not instead of it: `bn build start` holds the
foreground and waits on both ports, so the tunnel needs its own pane or window.
Ports 3000 and 3001 must already be listening — `ss -tln | grep -E ':(3000|3001)'`
— or the tunnel serves nothing and looks broken.

## Tailscale — your own machines

A private mesh: each machine gets a stable IP and name, nothing is exposed
publicly, and it survives reboots and network changes. Right answer for "reach my
desktop from my laptop", wrong answer for sharing with a colleague.

`cask "tailscale"` is in the dotfiles Brewfile for macOS. On Linux/WSL:

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
tailscale status              # names + IPs of your devices
```

Then reach the app at `http://<machine-name>:3000` from any device on the tailnet.

**On WSL specifically:** `.wslconfig` here sets `networkingMode=mirrored`, so WSL
shares the Windows host's network interfaces. A port bound inside WSL is reachable
at the Windows machine's address without the usual `netsh portproxy` dance — so
Tailscale installed on **Windows** already covers the WSL ports. Installing it
inside WSL as well is usually redundant; check the Windows side first.

## SSH forwarding — nothing to install

For a machine you can already SSH to, and only for yourself:

```bash
# pull a remote port to your local machine
ssh -N -L 3000:localhost:3000 user@remote

# push a local port out to a remote machine
ssh -N -R 3000:localhost:3000 user@remote     # needs GatewayPorts on the remote
```

`-N` means "no shell, just forward". It holds the terminal, so give it its own
pane and expect it to look idle — that is success, not a hang.

## Before declaring it working

Verify from the **other** side, not this one. A tunnel that resolves locally proves
nothing:

```bash
ss -tln | grep -E ':(3000|3001)'    # is the app even listening here?
curl -sS -o /dev/null -w '%{http_code}\n' <tunnel-url>
```

If the app isn't listening yet, fix that first — the tunnel is not the problem. See
the `onejs-build` skill for getting the BigMono dev server actually up.

## Rules

- **Never add `--allow-anonymous` without saying so.** It publishes the port.
- **Never tunnel a server holding real customer data** to an anonymous URL.
- Tunnels and forwards are long-running: give each its own named window, and warn
  that closing it drops the connection.
- Tear down when done — `devtunnel host` stops with Ctrl-C, and a stale tunnel is a
  live door into your machine.
