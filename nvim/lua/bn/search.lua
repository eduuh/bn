-- bn.search — an nvim surface over `bn search <q>`. The engine has full-text search across
-- every note + design doc, but recall ("where did I write about X") forced a drop to the
-- terminal. `:BnSearch <query>` lists the hits and `<CR>` opens the matching note at the line.
--
-- Telescope drives the picker when it's installed (snippet entries, file preview); otherwise
-- we fall back to `vim.ui.select` — which fzf-lua/dressing transparently take over when present.

local exec = require("bn.exec")

local M = {}

--- Open a hit's file at its line in the previous window, recentring the cursor.
local function open_hit(hit)
  if not hit or not hit.path then
    return
  end
  vim.cmd("edit " .. vim.fn.fnameescape(hit.path))
  local line = tonumber(hit.line) or 1
  pcall(vim.api.nvim_win_set_cursor, 0, { line, 0 })
  vim.cmd("normal! zz")
end

--- A one-line label for a hit: repo/branch, kind tag for docs, and the trimmed snippet.
local function label(hit)
  local where = (hit.repo or "?") .. "/" .. (hit.branch or "?")
  local tag = hit.kind == "doc" and " ⟨doc⟩" or ""
  return string.format("%s%s  %s", where, tag, vim.trim(hit.snippet or ""))
end

--- vim.ui.select fallback (also the path fzf-lua / dressing hook into).
local function pick_with_select(query, hits)
  vim.ui.select(hits, {
    prompt = "bn search: " .. query,
    format_item = label,
  }, function(hit)
    open_hit(hit)
  end)
end

--- Telescope picker: snippet as the display, file preview, `<CR>` opens at the line.
local function pick_with_telescope(query, hits)
  local pickers = require("telescope.pickers")
  local finders = require("telescope.finders")
  local conf = require("telescope.config").values
  local actions = require("telescope.actions")
  local action_state = require("telescope.actions.state")

  pickers
    .new({}, {
      prompt_title = "bn search: " .. query,
      finder = finders.new_table({
        results = hits,
        entry_maker = function(hit)
          return {
            value = hit,
            display = label(hit),
            ordinal = label(hit),
            filename = hit.path,
            lnum = tonumber(hit.line) or 1,
          }
        end,
      }),
      sorter = conf.generic_sorter({}),
      previewer = conf.grep_previewer({}),
      attach_mappings = function(prompt_bufnr)
        actions.select_default:replace(function()
          local entry = action_state.get_selected_entry()
          actions.close(prompt_bufnr)
          if entry then
            open_hit(entry.value)
          end
        end)
        return true
      end,
    })
    :find()
end

--- Run `bn search <query> --json` and hand the hits to whichever picker is available.
function M.search(query)
  query = query and vim.trim(query) or ""
  if query == "" then
    return vim.notify("bn: :BnSearch needs a query", vim.log.levels.WARN)
  end
  exec.json({ "search", query }, function(data)
    local hits = data and data.hits or {}
    if #hits == 0 then
      return vim.notify("bn: no matches for '" .. query .. "'", vim.log.levels.INFO)
    end
    local ok = pcall(require, "telescope")
    if ok then
      pick_with_telescope(query, hits)
    else
      pick_with_select(query, hits)
    end
  end)
end

--- Register `:BnSearch`. Called from `bn.setup()`.
function M.setup()
  vim.api.nvim_create_user_command("BnSearch", function(cmd)
    M.search(cmd.args)
  end, { nargs = "+", desc = "bn: search all notes" })
end

return M
