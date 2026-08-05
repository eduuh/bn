-- TOMORROW panel — the forward look at tomorrow's standup: today's still-open goals (they carry
-- forward when the day rolls over, ○) plus any scheduled follow-ups due ahead (→). Built from the
-- same `bn standup --json` read as STANDUP — no separate command. Detail shows the row; `↵` opens
-- a row's link.
--
-- Reads `bn standup --json` → uses `today[]` (carryover) and `upcoming[]` (scheduled ahead).

local exec = require("bn.exec")
local util = require("bn.util")

local M = {
  title = "TOMORROW",
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

  add("  Carrying forward")
  local carry = data.today or {}
  if #carry == 0 then
    add("    (nothing open — clean slate)")
  end
  for _, g in ipairs(carry) do
    add("    ○ " .. (g.text or ""), { kind = "goal", text = g.text, link = util.val(g.link) })
  end

  local upcoming = data.upcoming or {}
  if #upcoming > 0 then
    add("")
    add("  Scheduled ahead")
    for _, u in ipairs(upcoming) do
      add(string.format("    → %s  (due %s)", u.text or "", u.scheduled or "?"), {
        kind = "upcoming",
        text = u.text,
        link = util.val(u.link),
      })
    end
  end

  return lines, rows
end

M.detail_key = function(row)
  return "tomorrow:" .. (row.kind or "") .. ":" .. (row.text or "")
end

function M.detail(row, cb)
  local out = { "", "  " .. (row.text or "") }
  table.insert(out, "  " .. (row.kind == "upcoming" and "(scheduled follow-up)" or "(carries forward to tomorrow)"))
  if util.val(row.link) then
    table.insert(out, "")
    table.insert(out, "  link: " .. row.link)
    table.insert(out, "  (press ↵ to open · y to yank)")
  end
  cb(out)
end

M.keys = util.link_keys()

return M
