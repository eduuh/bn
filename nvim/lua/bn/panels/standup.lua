-- STANDUP panel — what you'd say at standup today: yesterday's done (✓) and undone (○), today's
-- plan (◷), and any open blockers (!). The TOMORROW panel shares the same `bn standup --json`
-- read for the forward look. Detail shows the selected row in full; `↵` opens a row's link.
--
-- Reads `bn standup --json` → { date, yesterday{date,completed[],incomplete[]}, today[], blockers[] }.

local exec = require("bn.exec")
local util = require("bn.util")

local M = {
  title = "STANDUP",
  footer = "↵ open link · y yank · r reload",
}

function M.fetch(cb)
  exec.json({ "standup" }, cb, { silent = true })
end

function M.render(data)
  local lines, rows, add = util.panel_rows()

  if data == nil then
    add("  ⚠ couldn't load `bn standup` — is bn up to date?")
    return lines, rows
  end

  -- One goal row: a glyph, its text, and the link/repo metadata the detail pane keys off.
  local function goal(g, mark)
    add("    " .. mark .. " " .. (g.text or ""), { kind = "goal", text = g.text, link = util.val(g.link) })
  end

  local y = data.yesterday
  add(string.format("  Yesterday · %s", (y and y.date) or "—"))
  local done, undone = (y and y.completed) or {}, (y and y.incomplete) or {}
  if #done == 0 and #undone == 0 then
    add("    (nothing recorded)")
  end
  for _, g in ipairs(done) do
    goal(g, "✓")
  end
  for _, g in ipairs(undone) do
    goal(g, "○")
  end

  add("")
  add("  Today")
  local today = data.today or {}
  if #today == 0 then
    add("    (no goals planned)")
  end
  for _, g in ipairs(today) do
    goal(g, "◷")
  end

  local blockers = data.blockers or {}
  if #blockers > 0 then
    add("")
    add("  Blockers")
    for _, b in ipairs(blockers) do
      add("    ! " .. (b.text or ""), {
        kind = "blocker",
        text = b.text,
        tag = (b.repo or "?") .. "/" .. (b.branch or "?"),
      })
    end
  end

  return lines, rows
end

M.detail_key = function(row)
  return "standup:" .. (row.kind or "") .. ":" .. (row.text or "")
end

function M.detail(row, cb)
  local out = { "", "  " .. (row.text or "") }
  if row.kind == "blocker" then
    table.insert(out, "  " .. (row.tag or ""))
  end
  if util.val(row.link) then
    table.insert(out, "")
    table.insert(out, "  link: " .. row.link)
    table.insert(out, "  (press ↵ to open · y to yank)")
  end
  cb(out)
end

M.keys = util.link_keys()

return M
