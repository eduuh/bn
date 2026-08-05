-- bn.dash — opens the lazybn cockpit. Kept as the stable entry point (`require("bn").world`);
-- the layout and panels live in bn.ui and bn.panels.*.

local ui = require("bn.ui")
local hl = require("bn.hl")
local brief = require("bn.panels.brief")
local staff = require("bn.panels.staff")

local M = {}

local panels = {
  require("bn.panels.next"),
  require("bn.panels.attn"),
  require("bn.panels.dispatch"),
  require("bn.panels.standup"),
  require("bn.panels.tomorrow"),
  require("bn.panels.cron"),
}

-- Compose the ordered panel list: BRIEF pinned above NEXT and STAFF at the end, but only when the
-- staff framework is installed (each probes synchronously at open — see their `available()`), so a
-- machine without the staff gets the exact cockpit it had before. Numbering stays dynamic in bn.ui.
-- Panel order: BRIEF · NEXT · ATTENTION · DISPATCH · STANDUP · TOMORROW · CRON · STAFF.
local function build_panels()
  local list = {}
  if brief.available() then
    table.insert(list, brief)
  end
  vim.list_extend(list, panels)
  if staff.available() then
    table.insert(list, staff)
  end
  return list
end

--- Open the cockpit — a detail pane on the right, the composed panel list on the left.
function M.world()
  hl.setup()
  ui.open(build_panels())
end

--- Open the cockpit focused on the DISPATCH panel — the live agent fleet, one keypress away.
function M.dispatch()
  hl.setup()
  ui.open(build_panels(), { focus = "DISPATCH" })
end

return M
