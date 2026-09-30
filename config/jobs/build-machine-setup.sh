#!/bin/sh
# build-machine-setup -- turn this machine into a remote build machine in one command.
#
# Run ON the new machine (WSL on a Cloud PC, say), after dotfiles set up bn, gh and the repo's git
# credentials:
#   BN_SETUP_REPO=BigMono BN_SETUP_URL=<clone url> bn run build-machine-setup
# (a private wrapper usually sets those; see the work jobs tier). Idempotent: every step checks
# first and says ok / done / NEEDS YOU, so re-running it is how you check a machine.
#
# Steps:
#   1. machine config: [machine] cron = true and the repo listed; [pool] scheduled includes it;
#      with BN_SETUP_WORK=1 the shared [work] work_repos lists it too (so bn finds the private
#      build script). Missing that sent AB12C's first warm to a scaffolded placeholder.
#   2. linger, so the user's systemd (and every bn timer) runs with nobody signed in;
#   3. bn's timers (`bn cron register`), with self-update paused again afterwards;
#   4. on WSL: the Windows keeper (wsl-keepalive) so the distro, tmux and tunnels stay up;
#   5. hosting the Dev Tunnel (tunnel-host), so the laptop finds it within a minute;
#   6. the repo's bare clone, shallow and blobless by default (BN_SETUP_CLONE_DEPTH /
#      BN_SETUP_CLONE_FILTER; the full BigMono history died after 12 GB on a flaky link);
# Pool builds then run on the machine's schedule. It prints the one thing left for a person, if any.
#
# Env: BN_SETUP_REPO (required), BN_SETUP_URL (required to clone), BN_SETUP_WORK=1,
#      BN_SETUP_BRANCH (default main), BN_SETUP_CLONE_DEPTH (default 1),
#      BN_SETUP_CLONE_FILTER (default blob:none).
set -u

REPO="${BN_SETUP_REPO:-}"
URL="${BN_SETUP_URL:-}"
BRANCH="${BN_SETUP_BRANCH:-main}"
DEPTH="${BN_SETUP_CLONE_DEPTH:-1}"
FILTER="${BN_SETUP_CLONE_FILTER:-blob:none}"
dir=$(cd "$(dirname "$0")" && pwd -P)
ME=$(hostname -s 2>/dev/null || hostname)
export PATH="$HOME/.local/bin:$PATH" XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

ok()    { echo "  ok    $*"; }
done_() { echo "  done  $*"; }
needs() { echo "  NEEDS YOU  $*"; left="$left
  - $*"; }
left=""
[ -n "$REPO" ] || { echo "build-machine-setup: set BN_SETUP_REPO (e.g. BigMono)" >&2; exit 2; }
case "$REPO$BRANCH" in *[!A-Za-z0-9_.-]*) echo "build-machine-setup: bad repo or branch name" >&2; exit 2 ;; esac
command -v bn >/dev/null 2>&1 || { echo "build-machine-setup: bn is not installed (run dotfiles setup first)" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "build-machine-setup: python3 is needed to edit bn's config" >&2; exit 1; }
echo "build-machine-setup: $ME for $REPO"

# --- 1. config --------------------------------------------------------------------
# bn has no config-set command; python3 edits the TOML without disturbing what is there, and only
# writes when something changes (a backup is kept next to the file).
notes="$HOME/projects/branch-notes"
edit_toml() { # <file> <python snippet over dict `d`>; prints "changed" when it wrote
  python3 - "$1" "$2" <<'PY'
import sys, tomllib, re, shutil, os
path, code = sys.argv[1], sys.argv[2]
text = open(path).read() if os.path.exists(path) else ""
d = tomllib.loads(text) if text else {}
before = repr(d)
exec(code)
if repr(d) == before:
    sys.exit(0)
def val(v):
    if isinstance(v, bool): return "true" if v else "false"
    if isinstance(v, list): return "[\n" + "".join(f'    "{x}",\n' for x in v) + "]"
    return f'"{v}"'
for table, kv in d.items():
    if not isinstance(kv, dict): continue
    for k, v in kv.items():
        line = f"{k} = {val(v)}"
        m = re.search(rf"(?ms)^\[{re.escape(table)}\]\s*$(.*?)(?=^\[|\Z)", text)
        if not m:
            text = text.rstrip("\n") + f"\n\n[{table}]\n{line}\n"
            continue
        body = m.group(1)
        km = re.search(rf"(?ms)^{re.escape(k)}\s*=\s*(\[.*?\]|[^\n]*)\n?", body)
        if km:
            body = body[:km.start()] + line + "\n" + body[km.end():]
        else:
            body = "\n" + line + body if not body.strip() else body.rstrip("\n") + "\n" + line + "\n\n"
        text = text[:m.start(1)] + body + text[m.end(1):]
if os.path.exists(path): shutil.copy(path, path + ".bak-setup")
open(path, "w").write(text)
tomllib.loads(text)
print("changed")
PY
}
mcfg="$notes/$ME/config.toml"
mkdir -p "$notes/$ME"
r=$(edit_toml "$mcfg" "
m = d.setdefault('machine', {})
m['cron'] = True
m['repos'] = sorted(set(m.get('repos', [])) | {'$REPO'})
p = d.setdefault('pool', {})
p['scheduled'] = sorted(set(p.get('scheduled', [])) | {'$REPO'})
") || { echo "build-machine-setup: could not edit $mcfg" >&2; exit 1; }
[ "$r" = changed ] && done_ "machine config: cron on, $REPO listed and scheduled ($mcfg)" || ok "machine config"
if [ "${BN_SETUP_WORK:-0}" = 1 ]; then
  lower=$(printf '%s' "$REPO" | tr 'A-Z' 'a-z')
  r=$(edit_toml "$notes/config.toml" "
w = d.setdefault('work', {})
w['work_repos'] = sorted(set(w.get('work_repos', [])) | {'$lower'})
") || { echo "build-machine-setup: could not edit $notes/config.toml" >&2; exit 1; }
  [ "$r" = changed ] && done_ "shared config: $lower in [work] work_repos" || ok "work repo routing"
fi

# --- 2. linger ----------------------------------------------------------------------
if [ "$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)" = yes ]; then
  ok "linger"
elif loginctl enable-linger "$(id -un)" < /dev/null 2>/dev/null; then
  done_ "linger enabled"
else
  needs "enable linger once: sudo loginctl enable-linger $(id -un)"
fi

# --- 3. timers --------------------------------------------------------------------
if bn cron register >/dev/null 2>&1; then
  bn cron unregister self-update >/dev/null 2>&1
  ok "bn timers registered (self-update paused)"
else
  needs "bn cron register failed; run it by hand to see why"
fi

# --- 4. WSL keeper ------------------------------------------------------------------
if grep -qi microsoft /proc/version 2>/dev/null; then
  sh "$dir/wsl-keepalive.sh" install >/dev/null 2>&1 && ok "WSL keeper (bn-wsl-keepalive task)" \
    || needs "wsl-keepalive failed; run: sh $dir/wsl-keepalive.sh install"
fi

# --- 5. tunnel ----------------------------------------------------------------------
if sh "$dir/tunnel-host.sh" < /dev/null > /tmp/bn-setup-tunnel.$$ 2>&1; then
  ok "hosting the Dev Tunnel ($(grep -o 'port [0-9]*\|127.0.0.1:[0-9]*\|0.0.0.0:[0-9]*' /tmp/bn-setup-tunnel.$$ | head -n 1))"
else
  needs "tunnel-host: $(tail -n 1 /tmp/bn-setup-tunnel.$$) (sign-in: ~/bin/devtunnel user login -g -d)"
fi
rm -f /tmp/bn-setup-tunnel.$$

# --- 6. clone -----------------------------------------------------------------------
bare="$HOME/projects/bare/$REPO.git"
if git --git-dir="$bare" rev-parse --verify -q "refs/remotes/origin/$BRANCH" >/dev/null 2>&1; then
  ok "$REPO cloned ($bare)"
elif [ -z "$URL" ]; then
  needs "no $REPO clone and no BN_SETUP_URL to clone it from"
else
  echo "  ...   cloning $REPO (depth $DEPTH, filter $FILTER); this takes a while"
  if (cd "$HOME/projects" && GCM_INTERACTIVE=never WT_CLONE_DEPTH="$DEPTH" WT_CLONE_FILTER="$FILTER" \
      WT_FETCH_REFSPEC="+refs/heads/$BRANCH:refs/remotes/origin/$BRANCH" bn wt clone "$URL" >/dev/null 2>&1); then
    done_ "$REPO cloned"
  else
    needs "$REPO clone failed; check its git credentials, then re-run this"
  fi
fi

if [ -z "$left" ]; then
  echo "build-machine-setup: $ME is ready. Pool builds run on its schedule; check with: bn run fleet (on the laptop)"
else
  echo "build-machine-setup: $ME needs you for:$left"
  exit 1
fi
