-- DISPATCH panel — the live agent fleet as tmux sees it: every dispatched worktree with a
-- running agent, ranked by who needs you, with a one-key jump that switches your tmux client
-- to that agent's window (workmux-sidebar style). tmux is the source of truth for state —
-- @bn_agent (working/waiting/done) per window, joined with session health.
--
-- Reads `bn agent live --json` → { agents: [ {repo, branch, target, harness, state, health,
-- idle_secs, has_window, tokens, turns, session_id} ], count, total_tokens }. `state` is the
-- Plane-A tmux decoration (falling back to Plane-B `health` when a window is undecorated), so
-- this reads exactly like `bn fleet` without leaving the editor.

local exec = require("bn.exec")
local util = require("bn.util")

local M = {
  title = "DISPATCH",
  footer = "↵ jump to window · r reload",
}

local val = util.val

-- Nerd Font marks, matched to `bn fleet`'s vocabulary. Built from codepoints so the bytes are
-- unambiguous; the plain geometric glyphs (health fallbacks) are BMP and safe as literals.
local function icon(cp)
  return vim.fn.nr2char(cp, true)
end

-- Plane-A `state` → { glyph, label, rank }. Lower rank sorts first (who-needs-you), mirroring
-- `board_rank` in agent_live.rs: needs-you states 0, stuck 1, working 2, done 3, idle 4, rest 5.
local STATE = {
  ["waiting"] = { glyph = icon(0xf0f3), label = "needs you", rank = 0 },
  ["limited"] = { glyph = icon(0xf254), label = "limited", rank = 0 },
  ["blocked"] = { glyph = icon(0xf05e), label = "blocked", rank = 0 },
  ["needs-decision"] = { glyph = "◆", label = "decide", rank = 0 },
  ["failed"] = { glyph = "✗", label = "failed", rank = 0 },
  ["working"] = { glyph = icon(0xf013), label = "working", rank = 2 },
  ["done"] = { glyph = icon(0xf00c), label = "done", rank = 3 },
}
-- Plane-B `health` fallback, for a window with no @bn_agent decoration (Copilot never fires it).
local HEALTH = {
  ["stuck"] = { glyph = "○", label = "stuck", rank = 1 },
  ["idle"] = { glyph = "◐", label = "idle", rank = 4 },
  ["active"] = { glyph = "·", label = "active", rank = 5 },
}
local UNKNOWN = { glyph = "·", label = "…", rank = 5 }

-- The display status for one agent row: its live tmux state wins; else its session health.
function M.status_of(a)
  return STATE[a.state or ""] or HEALTH[val(a.health)] or UNKNOWN
end

-- A compact token count, mirroring `fmt_tokens` in agent_live.rs: 950 → "950", 12300 → "12.3k".
local function fmt_tokens(n)
  if n < 1000 then
    return tostring(n)
  elseif n < 1000000 then
    return string.format("%.1fk", n / 1000)
  end
  return string.format("%.1fM", n / 1000000)
end
M.fmt_tokens = fmt_tokens

-- A raw seconds delta as a compact "Ns/m/h". Pure (unlike util.ago, which reads os.time), so the
-- detail pane stays deterministic for a given idle_secs.
local function fmt_dur(secs)
  if secs < 60 then
    return secs .. "s"
  elseif secs < 3600 then
    return math.floor(secs / 60) .. "m"
  end
  return math.floor(secs / 3600) .. "h"
end

function M.fetch(cb)
  exec.json({ "agent", "live" }, cb, { silent = true })
end

function M.render(data)
  local lines, rows, add = util.panel_rows()

  -- nil = the surface failed to load (vs. an empty fleet) — don't fake "all idle".
  if data == nil then
    add("  ⚠ couldn't load `bn agent live` — is bn up to date?")
    return lines, rows
  end

  local agents = data.agents or {}
  if #agents == 0 then
    add("  ✓ no live agents — nothing dispatched")
    return lines, rows
  end

  -- Rank who-needs-you first (mirrors the `bn fleet` board), stable by target within a rank.
  local ranked = vim.deepcopy(agents)
  table.sort(ranked, function(x, y)
    local rx, ry = M.status_of(x).rank, M.status_of(y).rank
    if rx ~= ry then
      return rx < ry
    end
    return (x.target or "") < (y.target or "")
  end)

  local waiting = 0
  for _, a in ipairs(ranked) do
    if M.status_of(a).rank == 0 then
      waiting = waiting + 1
    end
  end
  add(string.format("  %d agent%s · %d waiting", #ranked, #ranked == 1 and "" or "s", waiting))
  add("")

  for _, a in ipairs(ranked) do
    local st = M.status_of(a)
    local tok = (a.tokens and a.tokens > 0) and ("  " .. fmt_tokens(a.tokens) .. " tok") or ""
    local nowin = a.has_window and "" or "  (no window)"
    add(string.format("  %s %-9s %s%s%s", st.glyph, st.label, a.target or "?", tok, nowin), {
      target = val(a.target),
      repo = a.repo,
      branch = a.branch,
      harness = a.harness,
      state = a.state,
      health = val(a.health),
      idle_secs = a.idle_secs,
      tokens = a.tokens,
      turns = a.turns,
      has_window = a.has_window,
      session_id = val(a.session_id),
    })
  end
  return lines, rows
end

M.detail_key = function(row)
  return "dispatch:" .. (row.target or "")
end

function M.detail(row, cb)
  local st = M.status_of(row)
  local out = { "", "  " .. (row.target or "?"), "" }
  local function line(s)
    table.insert(out, "  " .. s)
  end
  line("state:    " .. st.glyph .. " " .. st.label .. ((val(row.state) == nil) and "  (from health)" or ""))
  line("harness:  " .. (val(row.harness) or "?"))
  if val(row.health) then
    line("health:   " .. row.health)
  end
  if val(row.idle_secs) then
    line("idle:     " .. fmt_dur(row.idle_secs))
  end
  if val(row.tokens) and row.tokens > 0 then
    line("tokens:   " .. fmt_tokens(row.tokens))
  end
  if val(row.turns) then
    line("turns:    " .. row.turns)
  end
  line("window:   " .. (row.has_window and "live  (↵ to jump)" or "none — session only"))
  cb(out)
end

-- Quick-jump to attach: switch this tmux client to the agent's session, then select its window.
-- Mirrors `goto` in jump.rs (switch-client + select-window). nvim runs inside tmux, so the
-- client switches in place; the cockpit stays open in its own window for when you switch back.
local function jump(row)
  local target = val(row and row.target)
  if not target then
    return
  end
  if not row.has_window then
    return vim.notify("bn: no live tmux window for " .. target .. " (session only)", vim.log.levels.INFO)
  end
  if not (vim.env.TMUX and vim.env.TMUX ~= "") then
    return vim.notify("bn: not inside tmux — can't jump", vim.log.levels.WARN)
  end
  local sess = target:match("^(.-):") or target
  vim.system({ "tmux", "switch-client", "-t", sess }, { text = true }, function(r1)
    vim.schedule(function()
      if r1.code ~= 0 then
        return vim.notify("bn: tmux switch-client failed: " .. vim.trim(r1.stderr or ""), vim.log.levels.ERROR)
      end
      -- Exact window match (=session:window), like agent_live.rs's `={target}`.
      vim.system({ "tmux", "select-window", "-t", "=" .. target }, { text = true }, function(r2)
        if r2.code ~= 0 then
          vim.schedule(function()
            vim.notify("bn: tmux select-window failed: " .. vim.trim(r2.stderr or ""), vim.log.levels.WARN)
          end)
        end
      end)
    end)
  end)
end

M.keys = {
  ["<cr>"] = function(_, row)
    if row then
      jump(row)
    end
  end,
  ["r"] = function(ctx)
    ctx.refresh()
  end,
}

return M
