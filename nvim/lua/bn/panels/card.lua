-- bn.panels.card — shared helpers for the BRIEF and STAFF panels, which both read the staff
-- framework's briefing cards (`bn staff feed --json`). Kept separate so the two panels agree on
-- what a card is, how it's rendered into the detail pane, and how the read-mark round-trips.
--
-- A `CardRef` (the feed index, `bn staff feed --json`) is { agent, date, headline, read, path }.
-- The full card at `path` is a BriefingCard { agent, date, headline, body_md, items[] } — the same
-- JSON `bn staff` writes. We read that file directly so *landing on* a card never marks it read;
-- only the explicit `m` key does, via `bn staff feed <agent>` (which writes the shared read.json).

local util = require("bn.util")

local M = {}

local val = util.val

--- The Chief of Staff files its card under agent `chief-am` / `chief-pm` (see bn_core::chief).
--- BRIEF pins the newest of those; everything else is a regular roster agent. Matched exactly so a
--- coincidentally-named agent (`chief-reviewer`, …) never masquerades as the Chief brief.
function M.is_chief(agent)
  agent = val(agent)
  return agent == "chief-am" or agent == "chief-pm"
end

-- Rank a chief card for "newest on its day": unread beats read, and PM (the 16:45 shutdown) beats
-- AM (the 05:30 brief) so the evening cockpit doesn't resurface the morning card.
local function chief_rank(c)
  return { c.read and 0 or 1, c.agent == "chief-pm" and 1 or 0 }
end

--- The newest Chief-of-Staff CardRef from a feed index (max date; then unread, then PM on a date
--- tie), or nil when the staff hasn't written one.
function M.newest_chief(cards)
  local best, best_rank
  for _, c in ipairs(cards or {}) do
    if M.is_chief(c.agent) then
      if not best or c.date > best.date then
        best, best_rank = c, chief_rank(c)
      elseif c.date == best.date then
        local r = chief_rank(c)
        if r[1] > best_rank[1] or (r[1] == best_rank[1] and r[2] > best_rank[2]) then
          best, best_rank = c, r
        end
      end
    end
  end
  return best
end

--- Every CardRef for one agent, newest date first — the history the STAFF panel pages with `[`/`]`.
function M.agent_cards(cards, agent)
  local out = {}
  for _, c in ipairs(cards or {}) do
    if c.agent == agent then
      table.insert(out, c)
    end
  end
  table.sort(out, function(a, b)
    return a.date > b.date
  end)
  return out
end

--- Read + decode the full BriefingCard JSON at a CardRef's `path`. Returns the card table, or nil
--- when the path is missing/absent/unparseable. Reading the file (vs. `bn staff feed <filter>`)
--- deliberately does NOT mark the card read.
function M.read_card_file(path)
  path = val(path)
  if not path then
    return nil
  end
  local ok, lines = pcall(vim.fn.readfile, path)
  if not ok or type(lines) ~= "table" then
    return nil
  end
  local decoded_ok, card = pcall(vim.json.decode, table.concat(lines, "\n"))
  if not decoded_ok or type(card) ~= "table" then
    return nil
  end
  return card
end

--- One item's display title, e.g. "[warn] review: Ship the harness".
function M.item_label(item)
  local sev = val(item.severity) or "info"
  local kind = val(item.kind) or "item"
  return string.format("[%s] %s: %s", sev, kind, val(item.title) or "")
end

--- Render a full BriefingCard into detail-pane lines: headline, byline, the body_md (the same
--- markdown the CLI's note detail renders — split and indented, no reflow), then its items. This
--- is the "reuse the note-detail md rendering" the detail pane already does elsewhere (cron output,
--- next detail): split on newlines and indent, letting the buffer's own wrap/syntax do the rest.
function M.detail_lines(card)
  if not card then
    return { "", "  (card unavailable)" }
  end
  local out = { "", "  " .. (val(card.headline) or "(untitled)") }
  local byline = table.concat({ val(card.agent) or "?", val(card.date) or "?" }, " · ")
  table.insert(out, "  " .. byline)
  table.insert(out, "")
  local body = val(card.body_md)
  if body then
    for _, l in ipairs(vim.split(body, "\n", { plain = true })) do
      table.insert(out, "  " .. l)
    end
  else
    table.insert(out, "  (no body)")
  end
  local items = card.items or {}
  if #items > 0 then
    table.insert(out, "")
    table.insert(out, "  ── items ──")
    for _, item in ipairs(items) do
      local line = "  • " .. M.item_label(item)
      if val(item.link) then
        line = line .. "  (" .. item.link .. ")"
      end
      table.insert(out, line)
    end
  end
  return out
end

return M
