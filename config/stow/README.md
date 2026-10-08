# config/stow

One stow package per tool; each mirrors `$HOME`:

| package | links into |
|---|---|
| `copilot` | `~/.copilot/` |
| `claude` | `~/.claude/` |
| `agency` | `~/.local/agency/` |
| `git` | `~/.config/git/` |

`install.sh` links them (GNU stow if installed, plain symlinks otherwise; no folding, so tool
state dirs never become symlinks into the repo). Real files you wrote are kept unless you pass
`--force-home`, which moves them to `<file>.bak-<time>`. Same flag moves unmanaged skills aside
so `bn skills install` can replace them.

Add a file at its home-relative path inside the right package and re-run `install.sh`.
Check with `bash scripts/check-install-stow.sh`.

Never put secrets or app-managed state here (`~/.copilot/config.json` holds auth tokens).
Per-machine settings belong in `~/.config/bn/`. Skills are not stowed: they are rendered per tool
from `config/prompts/` by `bn skills install`.
