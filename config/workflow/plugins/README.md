# Vendored tmux plugins

These are committed copies of the tmux plugins the workflow config uses, so bn is fully
self-contained — a fresh machine needs **no TPM and no network fetch**. `workflow/tmux.conf`
sources each plugin's entrypoint directly (resurrect before continuum); there is no plugin
manager involved.

| Plugin | Upstream | Vendored at commit |
|--------|----------|--------------------|
| `tmux-resurrect`  | https://github.com/tmux-plugins/tmux-resurrect  | `cff343cf9e81983d3da0c8562b01616f12e8d548` |
| `tmux-continuum`  | https://github.com/tmux-plugins/tmux-continuum  | `0698e8f4b17d6454c71bf5212895ec055c578da0` |

Each retains its upstream `LICENSE.md`. Non-runtime assets (`video/`, `tests/`, CI config)
were dropped to keep the tree lean.

## Updating

```sh
tmp=$(mktemp -d)
git clone --depth 1 https://github.com/tmux-plugins/tmux-resurrect "$tmp/r"
rm -rf tmux-resurrect && cp -r "$tmp/r" tmux-resurrect
rm -rf tmux-resurrect/.git tmux-resurrect/.github tmux-resurrect/video tmux-resurrect/tests
# repeat for tmux-continuum, then update the commit SHAs in the table above.
```
</content>
