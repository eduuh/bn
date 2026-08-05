-- :LazyBn (and the :Bn alias) open the lazybn cockpit — the same thing <leader>n does
-- after require("bn").setup().
if vim.g.loaded_bn_nvim then
  return
end
vim.g.loaded_bn_nvim = true

local function open()
  require("bn").world()
end

local function open_dispatch()
  require("bn").dash.dispatch()
end

vim.api.nvim_create_user_command("LazyBn", open, { desc = "lazybn: your world" })
vim.api.nvim_create_user_command("Bn", open, { desc = "lazybn: your world (alias)" })
vim.api.nvim_create_user_command("BnDispatch", open_dispatch, { desc = "lazybn: the DISPATCH fleet view" })

-- A <Plug> keymap for the cockpit (which now includes the BRIEF and STAFF panels). Exposed here
-- rather than as a hard-coded global map so it never clobbers a user binding — the opt-in default
-- `<leader>n` still lives in bn.setup(). Bind it with e.g. `vim.keymap.set("n", "<leader>b",
-- "<Plug>(bn-world)")`.
vim.keymap.set("n", "<Plug>(bn-world)", open, { desc = "lazybn: your world" })
