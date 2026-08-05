-- bn.quick — one-line capture without leaving your buffer. The human half of
-- self-maintaining notes: a prompt that writes a todo/blocker/decision to the current
-- repo/branch note via `bn add <section> <text>`, so logging never means leaving your work.
--
-- `:BnAdd` picks the section then prompts; the direct binds (`<leader>nt`/`nb`/`nd`)
-- skip straight to the prompt for a fixed section. No-ops gracefully outside a git repo,
-- since `bn add` resolves the note from the working directory.

local exec = require("bn.exec")

local M = {}

-- The sections `:BnAdd` offers, in the order the picker shows them.
local SECTIONS = { "todo", "blocker", "decision", "research", "collab", "ask" }

--- True when the current working directory sits inside a git repo. `bn add` resolves the
--- target note from the cwd, so outside a repo there's nothing to write to — we no-op.
local function in_repo()
  local out = vim.fn.systemlist({ "git", "rev-parse", "--is-inside-work-tree" })
  return vim.v.shell_error == 0 and out[1] == "true"
end

--- Prompt for one line, then `bn add <section> <text>`; notify on success. Empty/cancelled
--- input aborts silently.
local function capture(section)
  if not in_repo() then
    return vim.notify("bn: not in a git repo — nothing to add to", vim.log.levels.WARN)
  end
  vim.ui.input({ prompt = "bn " .. section .. ": " }, function(text)
    text = text and vim.trim(text) or ""
    if text == "" then
      return
    end
    exec.run({ "add", section, text }, function()
      vim.notify("bn: added " .. section .. " — " .. text, vim.log.levels.INFO)
    end)
  end)
end

M.capture = capture

--- `:BnAdd` — pick a section, then capture. An optional argument names the section directly
--- (`:BnAdd blocker`), skipping the picker.
function M.add(section)
  section = section and vim.trim(section) or ""
  if section ~= "" then
    if not vim.tbl_contains(SECTIONS, section) then
      return vim.notify("bn: unknown section '" .. section .. "'", vim.log.levels.ERROR)
    end
    return capture(section)
  end
  vim.ui.select(SECTIONS, { prompt = "bn add to which section?" }, function(choice)
    if choice then
      capture(choice)
    end
  end)
end

--- Register `:BnAdd` and (behind the keymaps flag) the direct section binds. Called from
--- `bn.setup()`.
function M.setup(opts)
  opts = opts or {}
  vim.api.nvim_create_user_command("BnAdd", function(cmd)
    M.add(cmd.args)
  end, {
    nargs = "?",
    desc = "bn: capture a note line",
    complete = function()
      return SECTIONS
    end,
  })
  if opts.keymaps ~= false then
    local function bind(lhs, section, label)
      vim.keymap.set("n", lhs, function()
        capture(section)
      end, { desc = "bn: add " .. label, silent = true })
    end
    bind("<leader>nt", "todo", "todo")
    bind("<leader>nb", "blocker", "blocker")
    bind("<leader>nd", "decision", "decision")
  end
end

return M
