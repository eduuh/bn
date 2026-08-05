-- BRIEF panel — the Chief of Staff's latest briefing, pinned at the top of the cockpit so it opens
-- with a narrative instead of only a queue. Renders the newest `chief-am`/`chief-pm` card
-- (`bn staff feed --json`): its headline plus the top-3 routed items as rows; the detail pane shows
-- the full `body_md`. `↵` follows the focused item's link, `m` marks the brief read (round-tripping
-- through `bn staff feed <agent>` so tmux and nvim agree), and once today's brief is read the panel
-- collapses to a single dim line — the cockpit shouldn't re-shout old news.
--
-- The panel is present at all only when a Chief card exists (see `available()`), so pre-framework
-- machines and quiet weekends pay nothing for it.

local exec = require("bn.exec")
local util = require("bn.util")
local card = require("bn.panels.card")

local M = {
  title = "BRIEF",
  footer = "↵ follow item · m mark read · r reload",
}

local val = util.val

-- Top-3 only: the brief is a headline, not the full queue (that's NEXT's job).
local TOP_N = 3

local function item_glyph(sev)
  return sev == "urgent" and "" or sev == "warn" and "" or "·"
end

--- Is a Chief brief present right now? Decides whether the cockpit mounts this panel at all — a
--- synchronous probe run once at open time (the framework being installed is stable within a
--- session). Absent ⇒ the panel is never built, so there's nothing to hide or dim.
function M.available()
  local data = exec.json_sync_raw({ "staff", "feed" })
  return data ~= nil and card.newest_chief(data.cards) ~= nil
end

--- Resolve the newest Chief card and load its full body/items once per refresh (reading the file
--- never marks it read), so `render` and `detail` stay pure projections of the fetched data.
function M.fetch(cb)
  exec.json_raw({ "staff", "feed" }, function(data)
    if data == nil then
      return cb(nil)
    end
    local ref = card.newest_chief(data.cards)
    cb({ ref = ref, full = ref and card.read_card_file(ref.path) or nil })
  end, { silent = true })
end

function M.render(data)
  local lines, rows, add = util.panel_rows()

  if data == nil then
    add("  ⚠ couldn't load `bn staff feed` — is bn up to date?")
    return lines, rows
  end
  local ref = data.ref
  if not ref then
    add("  (no brief today)")
    return lines, rows
  end

  -- Read ⇒ collapse to one dim line; the brief is old news the moment it's been read.
  if ref.read then
    add("  ✓ " .. (val(ref.headline) or "brief") .. "  · read", { ref = ref, full = data.full })
    return lines, rows
  end

  -- Unread: the headline, then the top-3 routed items as follow-able rows.
  add("  " .. (val(ref.headline) or "(untitled brief)"), { ref = ref, full = data.full })
  local items = (data.full and data.full.items) or {}
  for i = 1, math.min(TOP_N, #items) do
    local it = items[i]
    add(
      string.format("  %d %s %s", i, item_glyph(val(it.severity) or "info"), val(it.title) or ""),
      { ref = ref, full = data.full, link = val(it.link) }
    )
  end
  return lines, rows
end

M.detail_key = function(row)
  return "brief:" .. (row.ref and (row.ref.agent .. "@" .. row.ref.date) or "none") .. ":" .. (row.link or "")
end

--- The detail pane is always the full card body — the same for the headline row and each item row,
--- so the narrative stays on screen while you move between its items.
function M.detail(row, cb)
  local full = row.full or (row.ref and card.read_card_file(row.ref.path))
  cb(card.detail_lines(full))
end

M.keys = {
  -- Follow the focused item's link; on the headline row there's nothing to follow.
  ["<cr>"] = function(_, row)
    if row and row.link then
      util.open_link(row.link)
    else
      vim.notify("bn: no link to follow on this line", vim.log.levels.INFO)
    end
  end,
  -- Mark the brief read through the CLI so the read.json the tmux side reads stays in sync.
  ["m"] = function(ctx, row)
    if not (row and row.ref) then
      return
    end
    if row.ref.read then
      return vim.notify("bn: brief already read", vim.log.levels.INFO)
    end
    exec.run({ "staff", "feed", row.ref.agent }, function()
      ctx.refresh()
    end)
  end,
  ["r"] = function(ctx)
    ctx.refresh()
  end,
}

return M
