-- Headless test bootstrap for the lazybn cockpit specs.
--
-- Run with:
--   nvim --headless -u nvim/tests/minimal_init.lua \
--     -c "PlenaryBustedDirectory nvim/tests/ { minimal_init = 'nvim/tests/minimal_init.lua' }"
-- or just: nvim/tests/run.sh
--
-- plenary.nvim is the only test dependency. It is cloned on demand into
-- nvim/tests/.deps/ (gitignored) so the harness is self-contained — no global
-- plugin manager required, on a dev box or on CI.

local this = debug.getinfo(1, "S").source:sub(2)
local tests_dir = vim.fn.fnamemodify(this, ":p:h") -- nvim/tests
local nvim_dir = vim.fn.fnamemodify(tests_dir, ":h") -- nvim
-- Cache the dep OUTSIDE nvim/tests/ so PlenaryBustedDirectory's recursive
-- *_spec.lua glob never picks up plenary's own test suite.
local deps = nvim_dir .. "/.tests-deps"
local plenary = deps .. "/plenary.nvim"

if vim.fn.isdirectory(plenary) == 0 then
  vim.fn.mkdir(deps, "p")
  io.stderr:write("bootstrap: cloning plenary.nvim into " .. plenary .. "\n")
  local out = vim.fn.system({
    "git",
    "clone",
    "--filter=blob:none",
    "--depth=1",
    "https://github.com/nvim-lua/plenary.nvim",
    plenary,
  })
  if vim.v.shell_error ~= 0 then
    io.stderr:write("bootstrap: failed to clone plenary.nvim\n" .. out .. "\n")
    os.exit(1)
  end
end

-- Put the bn plugin (nvim/lua/bn) and plenary on the runtimepath so
-- `require("bn.*")` and `require("plenary.*")` resolve.
vim.opt.runtimepath:prepend(nvim_dir)
vim.opt.runtimepath:prepend(plenary)

vim.cmd("runtime plugin/plenary.vim")
