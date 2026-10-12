# Vendored tmux plugins

These are committed runtime copies of the tmux plugins the workflow config uses.
`workflow/tmux.conf` sources each entrypoint directly (resurrect before continuum):
**no TPM and no reload-time downloads**. Fingers additionally uses an installed
binary; full-mode dependency setup downloads the pinned 2.7.1 release and verifies
its SHA256. Python 3 and fzf are full-workflow dependencies.

| Plugin | Upstream | Vendored at commit |
|--------|----------|--------------------|
| `tmux-resurrect`  | https://github.com/tmux-plugins/tmux-resurrect  | `cff343cf9e81983d3da0c8562b01616f12e8d548` |
| `tmux-continuum`  | https://github.com/tmux-plugins/tmux-continuum  | `0698e8f4b17d6454c71bf5212895ec055c578da0` |
| `tmux-logging` | https://github.com/tmux-plugins/tmux-logging | `b5c5f7b9bc679ca161a442e932d6186da8d3538f` |
| `tmux-fingers` (loader) | https://github.com/Morantron/tmux-fingers | `fc3c750b8d73ac3e29675aae3b3ac6a00a0718f1` (2.7.1) |
| `extrakto` | https://github.com/laktak/extrakto | `d1af77988081dae496fa4a1f5e5e6bc9ef66767f` |
| `tmux-open` | https://github.com/tmux-plugins/tmux-open | `763d0a852e6703ce0f5090a508330012a7e6788e` |

Each retains its upstream license. Non-runtime assets (`video/`, `tests/`, CI config)
were dropped to keep the tree lean.

## Local integration patches

- Logging binds only recording, visible capture and retained-history export, with
  the pane ID captured at invocation. It never starts recording automatically or
  binds the destructive clear-history convenience key. A live pipe is the source
  of truth; an unrelated pipe is not replaced. Files get unique names and `0600`
  permissions in a `0700` directory. Shell paths are quoted and output failures
  propagate. See `scripts/check-tmux-logging.py`.
- Fingers retains its upstream loader and license, not source-build or wizard
  assets. The loader finds the installed binary (including `~/.local/bin`) and
  reports a missing dependency instead of downloading or opening a wizard.
  `scripts/install-tmux-fingers.sh` holds version/platform/checksum pins.
- Extrakto keeps its CLI, filters and popup runtime. Its local patches preserve
  PATH lookup, capture all requested scrollback, safely pass selected text to
  open/editor commands, and clean up the fzf pipe. Copies use `tmux_osc52`.
- Selected-text opening uses `workflow/bin/tmux-open-selection.py` with selection
  on stdin, never substituted into shell syntax. Browser/file opening goes through
  `bn open`, searches are URL-encoded, and editor opening creates a separate popup
  instead of typing a command into a running agent. Search-key parsing supports
  dashed names such as `M-s`. Selection-end and clipboard-copy keys are preserved.

Fingers binaries are provided upstream for Linux x86_64 and macOS arm64. Other
platforms can install upstream themselves; the full installer warns explicitly if
the hint tool is unavailable. `BN_INSTALL_DEPS=0` skips dependency installation.

## Updating

Fetch the chosen upstream revision into a temporary checkout, copy only runtime
files and its license, reapply the documented integration patches, and update the
SHA table. For fingers, update both the loader revision and the release digests
in the installer; never download from a key binding. Run the focused plugin,
logging and installer checks documented in `workflow/docs/tmux.md`.
