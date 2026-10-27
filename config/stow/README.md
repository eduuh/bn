# config/stow

One stow package per tool; each mirrors `$HOME`:

| package | links into |
|---|---|
| `copilot` | `~/.copilot/` |
| `claude` | `~/.claude/` |
| `git` | `~/.config/git/` |

`install.sh` links them (GNU stow if installed, plain symlinks otherwise; no folding, so tool
state dirs never become symlinks into the repo). Real files you wrote are kept unless you pass
`--force-home`, which moves them to `<file>.bak-<time>`. Same flag moves unmanaged skills aside
so `bn skills install` can replace them.

`copilot` also carries the static `hooks/bn-model.json`, `hooks/bn-guard.json` (git guard,
`workflow/bin/guard-git.sh`), `hooks/bn-session.json`, `lsp-config.json`, `instructions/*.instructions.md`.
`claude` wires the same guard as a `PreToolUse` hook plus a
`statusLine`. `~/.copilot/hooks/bn-agent.json` is written by `bn agent launch`
and is never tracked.

Add a file at its home-relative path inside the right package and re-run `install.sh`.
Check with `bash scripts/check-install-stow.sh`.

Repository-only files such as `.github/workflows/ci.yml` stay in bn's Git checkout, not a stow
package or `$HOME`. The installed source anchor points to the full stable checkout; `bn self-update`
fast-forwards that checkout. Existing stow links follow source edits, but new home files need a
full `install.sh` run. Generated skills need `bn skills install` to refresh their harness copies.

Never put secrets or app-managed state here (`~/.copilot/config.json` holds auth tokens).
Per-machine settings belong in `~/.config/bn/`. Skills are not stowed: they are rendered per tool
from `config/prompts/` by `bn skills install`.
