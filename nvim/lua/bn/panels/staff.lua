-- STAFF panel — the agent roster and its health, below CRON. One row per roster agent
-- (`bn staff --json`): an unread dot, the agent, its schedule, and the last run's health/age —
-- exactly the CRON idiom, but the health is the staff run's, not a systemd unit's. The detail pane
-- renders that agent's latest briefing card (`bn staff feed --json`), and `[`/`]` page back through
-- its card history.
--
-- Keys: `↵` (re)open the latest card in detail · `R` run the agent now · `P` plan (dry-run in the
-- detail pane) · `+`/`-` up/down-rate the shown card (`bn feed rate`, #268) · `e` edit the agent's
-- skill prompt · `m` mark the agent's cards read (round-tripping through `bn staff feed <agent>`).
-- Present only when the staff framework is installed (see `available()`), so nothing changes for
-- users without it.

local exec = require("bn.exec")
local util = require("bn.util")
local card = require("bn.panels.card")

local M = {
  title = "STAFF",
  footer = "↵ open card · R run · P plan · +/- rate · e edit skill · m mark read · [ ] history",
}

local val = util.val

-- Per-agent detail state, kept on the module (the panel table is a shallow copy, but these
-- closures capture the module, so the state survives): which card of the history we're paged to,
-- the cached dry-run plan, and whether the detail pane is currently showing that plan.
M._page = {}
M._plan = {}
M._show_plan = nil

-- The CardRef currently shown in the detail pane for a row (the one paged to via `[`/`]`), or nil
-- when the agent has no cards yet. Mirrors detail()'s idx clamp so a rating hits the visible card.
local function current_card(row)
  local cards = row and row.cards or {}
  if #cards == 0 then
    return nil
  end
  local idx = math.max(1, math.min(#cards, M._page[row.name] or 1))
  return cards[idx]
end

-- Humanize a systemd OnCalendar expression into the short cockpit form the spec's STAFF mock uses
-- (`06:30`, `Sat`), so the schedule column reads like the CRON idiom rather than raw systemd
-- grammar. A weekday-scoped schedule shows the weekday token (`Sat`, `Mon..Fri`, `Sat,Sun`) — the
-- salient fact — and drops the `*-*-* HH:MM:SS` tail; a plain daily one shows just its `HH:MM`.
-- An absent schedule reads as `manual`; an unrecognized shape passes through raw (a systemd token
-- beats a blank column).
local function humanize_schedule(oncal)
  oncal = val(oncal)
  if not oncal then
    return "manual"
  end
  -- Leading weekday token (systemd DOW: `Sat`, `Mon..Fri`, `Sat,Sun`, and shorthand like `daily`).
  local dow = oncal:match("^%s*(%a%a%a[%a.,%-]*)")
  if dow then
    return dow
  end
  -- Otherwise a daily time — surface the `HH:MM` from `*-*-* 06:30:00`.
  local hh, mm = oncal:match("(%d%d):(%d%d)")
  if hh then
    return hh .. ":" .. mm
  end
  return oncal
end
M._humanize_schedule = humanize_schedule

-- Lead glyph: a missed or failed last run flags itself (✗); otherwise the unread dot (● / ○).
local function lead_glyph(a)
  local h = val(a.health) or val(a.last_status)
  if h == "missed" or h == "fail" or h == "auth" then
    return "✗"
  end
  return a.unread_card and "●" or "○"
end

-- Health/age column, spelling the #273 taxonomy: "missed <ago>" for a scheduled run that never
-- fired (silence = death), "silent <ago>" for a by-design quiet day (ran, said nothing), "✓ <ago>"
-- for a card, the raw status word for a bad run (fail/auth/rate-limited/skipped), "never" when it
-- has not run. Falls back to the pre-#273 `last_status` field for an older CLI.
local function health_col(a)
  local h = val(a.health)
  if not h then
    local st = val(a.last_status)
    if not st then
      return "never"
    end
    return st == "ok" and ("✓ " .. util.ago(a.last_ts)) or st
  end
  if h == "never" then
    return "never"
  elseif h == "missed" then
    return "missed " .. util.ago(val(a.missed_since))
  elseif h == "ok-silent" then
    return "silent " .. util.ago(a.last_ts)
  elseif h == "ok-with-card" then
    return "✓ " .. util.ago(a.last_ts)
  end
  return h -- fail / auth / rate-limited / skipped
end

--- Is the staff framework installed here? Decides whether the cockpit mounts this panel at all — a
--- one-shot synchronous probe at open time. Any declared agent or any written card counts.
function M.available()
  local sd = exec.json_sync_raw({ "staff" })
  if sd and sd.agents and #sd.agents > 0 then
    return true
  end
  local fd = exec.json_sync_raw({ "staff", "feed" })
  return fd ~= nil and fd.cards ~= nil and #fd.cards > 0
end

--- Roster + health (`bn staff`) joined with the card index (`bn staff feed`) — two bare-payload
--- reads chained so a row can carry its agent's card history for the detail pane.
function M.fetch(cb)
  exec.json_raw({ "staff" }, function(sd)
    exec.json_raw({ "staff", "feed" }, function(fd)
      if sd == nil and fd == nil then
        return cb(nil)
      end
      cb({ agents = (sd and sd.agents) or {}, cards = (fd and fd.cards) or {} })
    end, { silent = true })
  end, { silent = true })
end

function M.render(data)
  local lines, rows, add = util.panel_rows()

  if data == nil then
    add("  ⚠ couldn't load `bn staff` — is bn up to date?")
    return lines, rows
  end

  local agents = data.agents or {}
  add(string.format("  %d agent%s", #agents, #agents == 1 and "" or "s"))
  add("")
  if #agents == 0 then
    add("  no staff agents declared (see config.example/agents.toml)")
    return lines, rows
  end

  for _, a in ipairs(agents) do
    local meta = vim.tbl_extend("force", {}, a)
    meta.cards = card.agent_cards(data.cards, a.name)
    local sched = humanize_schedule(a.on_calendar)
    add(string.format("  %s %-14s %-12s %s", lead_glyph(a), a.name, sched, health_col(a)), meta)
  end
  return lines, rows
end

M.detail_key = function(row)
  local tag = (M._show_plan == row.name) and "plan" or ("card" .. tostring(M._page[row.name] or 1))
  return "staff:" .. (val(row.name) or "?") .. ":" .. tag
end

-- The dry-run plan, rendered into the detail pane by `P`.
local function plan_lines(name, plan)
  local out = { "", "  " .. name .. " — plan (dry-run, nothing lands)", "" }
  local function line(s)
    table.insert(out, "  " .. s)
  end
  if not plan then
    line("(plan failed — is this agent declared in agents.toml?)")
    return out
  end
  line("gather:  " .. (val(plan.gather) or "?"))
  line("skill:   " .. (val(plan.skill) or "?") .. (plan.skill_found == false and "   (NOT FOUND)" or ""))
  line("harness: " .. (val(plan.harness) or "?"))
  local sinks = plan.sinks or {}
  line("sinks:   " .. (#sinks > 0 and table.concat(sinks, ", ") or "(none declared)"))
  table.insert(out, "")
  table.insert(out, "  ── packet ──")
  if val(plan.packet) ~= nil then
    for _, l in ipairs(vim.split(vim.inspect(plan.packet), "\n", { plain = true })) do
      table.insert(out, "  " .. l)
    end
  elseif val(plan.gather_error) then
    line("gather error: " .. plan.gather_error)
  else
    line("(no packet gathered)")
  end
  table.insert(out, "")
  table.insert(out, "  (press ↵ to return to cards)")
  return out
end

--- Detail = this agent's latest card (or the one paged to), with a header showing where in the
--- history we are; or the dry-run plan when `P` asked for it.
function M.detail(row, cb)
  if M._show_plan == row.name then
    return cb(plan_lines(row.name, M._plan[row.name]))
  end

  local cards = row.cards or {}
  local out = { "", "  " .. row.name }
  local function meta(s)
    table.insert(out, "  " .. s)
  end
  meta("schedule: " .. humanize_schedule(row.on_calendar))
  meta("harness:  " .. (val(row.harness) or "copilot") .. "   scope: " .. (val(row.scope) or "?"))
  if val(row.last_error) then
    meta("last err: " .. row.last_error)
  end

  if #cards == 0 then
    table.insert(out, "")
    meta("(no cards yet — each run writes one)")
    return cb(out)
  end

  local idx = math.max(1, math.min(#cards, M._page[row.name] or 1))
  M._page[row.name] = idx
  local ref = cards[idx]
  table.insert(out, "")
  table.insert(out, string.format("  ── card %d/%d · %s ──", idx, #cards, ref.date))
  vim.list_extend(out, card.detail_lines(card.read_card_file(ref.path)))
  cb(out)
end

-- Page through the agent's card history: delta +1 older, -1 newer. Clears any plan view first, then
-- re-renders the detail (a cheap local re-read).
local function page(ctx, row, delta)
  if not row or #(row.cards or {}) == 0 then
    return
  end
  M._show_plan = nil
  local idx = (M._page[row.name] or 1) + delta
  M._page[row.name] = math.max(1, math.min(#row.cards, idx))
  ctx.refresh()
end

-- Resolve an agent's skill file and open it. The skill name isn't on `bn staff --json`, so we read
-- it from the (cached) dry-run plan, then look the file up in `bn skills list`.
local function edit_skill(ctx, name)
  local function open_for(plan)
    local skill = plan and val(plan.skill)
    if not skill then
      return vim.notify("bn: couldn't resolve " .. name .. "'s skill", vim.log.levels.WARN)
    end
    exec.json({ "skills", "list" }, function(sd)
      for _, s in ipairs((sd and sd.skills) or {}) do
        if s.name == skill and val(s.path) then
          return ctx.open(s.path)
        end
      end
      vim.notify(
        "bn: skill '" .. skill .. "' has no editable file here — try `bn skills edit " .. skill .. "`",
        vim.log.levels.WARN
      )
    end, { silent = true })
  end

  if M._plan[name] then
    return open_for(M._plan[name])
  end
  exec.json_raw({ "staff", "plan", name }, function(plan)
    M._plan[name] = plan
    open_for(plan)
  end, { silent = true, accept_nonzero_json = true })
end

-- Up/down-rate the card currently shown for this agent (`bn feed rate <date>/<agent> up|down`).
-- The rating marks it read, so the refresh clears the unread dot in place. A no-op (with a notify)
-- when the agent has no card yet — there is nothing to react to.
local function rate(ctx, row, direction)
  if not row then
    return
  end
  local ref = current_card(row)
  if not ref then
    return vim.notify("bn: no card to rate for " .. (val(row.name) or "?"), vim.log.levels.WARN)
  end
  M._show_plan = nil
  exec.run({ "feed", "rate", (val(ref.date) or "") .. "/" .. row.name, direction }, function()
    ctx.refresh()
  end)
end

M.keys = {
  -- Return the detail pane to this agent's latest card.
  ["<cr>"] = function(ctx, row)
    if not row then
      return
    end
    M._show_plan = nil
    M._page[row.name] = 1
    ctx.refresh()
  end,
  ["["] = function(ctx, row)
    page(ctx, row, 1)
  end,
  ["]"] = function(ctx, row)
    page(ctx, row, -1)
  end,
  ["R"] = function(ctx, row)
    if not row then
      return
    end
    M._show_plan = nil
    vim.notify("bn: running staff " .. row.name .. "…", vim.log.levels.INFO)
    exec.run({ "staff", "run", row.name }, function()
      ctx.refresh()
    end)
  end,
  ["P"] = function(ctx, row)
    if not row then
      return
    end
    vim.notify("bn: planning " .. row.name .. "… (dry-run)", vim.log.levels.INFO)
    exec.json_raw({ "staff", "plan", row.name }, function(plan)
      M._plan[row.name] = plan
      M._show_plan = row.name
      ctx.refresh()
    end, { silent = true, accept_nonzero_json = true })
  end,
  ["+"] = function(ctx, row)
    rate(ctx, row, "up")
  end,
  ["-"] = function(ctx, row)
    rate(ctx, row, "down")
  end,
  ["e"] = function(ctx, row)
    if row then
      edit_skill(ctx, row.name)
    end
  end,
  ["m"] = function(ctx, row)
    if not row then
      return
    end
    exec.run({ "staff", "feed", row.name }, function()
      ctx.refresh()
    end)
  end,
}

return M
