-- bn.nvim — lazybn, a lazygit-style cockpit for your bn world.
--
-- `<leader>n` (or `:LazyBn` / `:Bn`) opens a full-screen, keyboard-driven board:
-- the NEXT action feed, the ATTENTION list, the DISPATCH fleet (live agents by tmux state),
-- today's STANDUP, the forward look at TOMORROW, and CRON jobs across repos (with the info
-- they gathered) — list panels on the left, a detail pane on the right. Navigate with
-- <Tab>/1-N, act with the panel's contextual keys (shown in the footer), jump into a note or
-- cron script with <CR>/e. `<leader>nd` (or `:BnDispatch`) opens straight to DISPATCH.

local exec = require("bn.exec")
local dash = require("bn.dash")
local status = require("bn.status")
local quick = require("bn.quick")
local search = require("bn.search")

local M = {
  exec = exec,
  dash = dash,
  status = status,
  quick = quick,
  search = search,
  world = dash.world,
}

function M.setup(opts)
  opts = opts or {}
  exec.setup(opts)
  quick.setup(opts)
  search.setup()
  if opts.status then
    status.setup(opts.status)
  end
  if opts.keymaps ~= false then
    vim.keymap.set("n", "<leader>n", dash.world, { desc = "lazybn: your world", silent = true })
    vim.keymap.set("n", "<leader>nd", dash.dispatch, { desc = "lazybn: dispatch fleet", silent = true })
  end
end

return M
