-- NEXT panel — the "what do I do now?" list: one ranked action feed folding failing cron jobs,
-- raised attention items, PRs awaiting your review, your stale in-flight PRs, today's open goals,
-- and blocked notes, highest-priority first. Where ATTENTION/CRON show what's wrong, NEXT shows
-- what to act on. Reads `bn next --json`; `<CR>` opens the focused action's link.

local exec = require("bn.exec")
local util = require("bn.util")

local M = {
  title = "NEXT",
  footer = "↵ open link · y yank url · r reload",
}

local val = util.val

function M.fetch(cb)
  exec.json({ "next" }, cb, { silent = true })
end

function M.render(data)
  local lines, rows, add = util.panel_rows()

  -- nil = the surface failed to load (vs. an empty action list) — don't fake "all clear".
  if data == nil then
    add("  ⚠ couldn't load `bn next` — is bn up to date?")
    return lines, rows
  end

  local actions = data.actions or {}
  if #actions == 0 then
    add("  ✓ nothing queued — desk is clear")
    return lines, rows
  end

  for i, a in ipairs(actions) do
    local g = util.next_glyph(a.kind)
    add(string.format("  %d %s %s", i, g, a.title or ""), {
      kind = a.kind,
      title = a.title,
      detail = a.detail,
      link = val(a.link),
    })
  end

  return lines, rows
end

M.detail_key = function(row)
  return "next:" .. (row.title or "")
end

-- Detail is synchronous — the action's title, its (possibly multi-line) detail, and link.
function M.detail(row, cb)
  local out = { "", "  " .. (row.title or "") }
  if row.detail and row.detail ~= "" then
    table.insert(out, "")
    -- detail can be a multi-line write-up (e.g. a review's reasoning) — render each line.
    for line in (row.detail .. "\n"):gmatch("(.-)\n") do
      table.insert(out, "  " .. line)
    end
  end
  if row.link then
    table.insert(out, "")
    table.insert(out, "  link: " .. row.link)
    table.insert(out, "  (press ↵ to open)")
  end
  cb(out)
end

M.keys = util.link_keys()

return M
