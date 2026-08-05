-- bn.status — a cached status string for a statusline (lualine et al). The component getter
-- returns the cache synchronously; `refresh` updates it async so the UI never blocks.

local exec = require("bn.exec")
local util = require("bn.util")

local M = {}

-- Append the top `bn next` action to `component()` when set. Off by default so the plain
-- counts string is unchanged; flip it via `require("bn.status").setup({ next = true })` or
-- compose `next_component()` into your own lualine section directly.
M.config = { next = false }

function M.setup(opts)
  M.config = vim.tbl_extend("force", M.config, opts or {})
end

local cache = ""
-- A statusline redraws constantly (every cursor move); spawning `bn status` each time would
-- be dozens of subprocesses a second. So `component()` throttles: it kicks off at most one
-- async refresh per interval and otherwise serves the last good value instantly.
local TTL_MS = 5000
local last_refresh = nil

--- Async-refresh the cached component from `bn status`. Empty when the dir has no note.
function M.refresh()
  last_refresh = vim.uv.now()
  exec.json({ "status" }, function(d)
    if d and d.open_todos ~= nil then
      local pr = (d.pr and d.pr ~= vim.NIL) and " PR" or ""
      cache = string.format("bn %s ✓%d !%d%s", d.branch or "?", d.open_todos or 0, d.blockers or 0, pr)
    else
      cache = ""
    end
  end)
end

-- The top-action component is its own throttled cache: `bn next` is a heavier, cross-repo
-- query, so it gets a separate TTL and never piggybacks on the per-branch status refresh.
local next_cache = ""
local next_last_refresh = nil

-- Focus mode (#119): while a deep-work block is active the statusline shows the block (" focus
-- · 47m left") in place of the next-action — the quiet window made visible. Cached and refreshed
-- on the same tick as the next-action so the two stay coherent.
local focus_cache = ""

--- Async-refresh the cached focus-block label from `bn focus status`. Empty when no block is
--- active or the surface fails (an older binary without `bn focus`), so callers fall back to the
--- next-action.
function M.focus_refresh()
  exec.json({ "focus", "status" }, function(d)
    if d and d.active and d.block then
      local mins = math.ceil((d.block.remaining_secs or 0) / 60)
      focus_cache = string.format(" focus · %dm left", mins)
    else
      focus_cache = ""
    end
  end, { silent = true })
end

--- Async-refresh the cached top-action string. Inside a focus block this is the block label
--- (" focus · 47m left"); otherwise it's the #1 ranked `bn next` action with its kind glyph.
--- Empty when nothing is queued or the surface fails — callers fall back to counts.
function M.next_refresh()
  next_last_refresh = vim.uv.now()
  M.focus_refresh()
  exec.json({ "next" }, function(d)
    local top = d and d.actions and d.actions[1]
    if top then
      local g = util.next_glyph(top.kind)
      next_cache = string.format("%s %s", g, top.title or "")
    else
      next_cache = ""
    end
  end, { silent = true })
end

--- The cached top-`bn next` action string (glyph + title), for users who want to drop the
--- single most useful "keep me on top" signal into their own lualine section. Throttled like
--- `component()` so frequent redraws stay free; empty when the desk is clear. Inside a focus
--- block it shows the block (" focus · 47m left") instead — the quiet window suppresses the
--- nudge, so the statusline reflects the protected block, not the next action.
function M.next_component()
  if not next_last_refresh or (vim.uv.now() - next_last_refresh) >= TTL_MS then
    M.next_refresh()
  end
  if focus_cache ~= "" then
    return focus_cache
  end
  return next_cache
end

--- The cached component string (for `lualine` sections). Refreshes at most once per TTL so
--- frequent redraws stay free; the first refresh's result lands a beat later. With
--- `config.next`, appends the top `bn next` action; falls back to the counts alone when the
--- desk is clear, and to the empty string when there's no note.
function M.component()
  if not last_refresh or (vim.uv.now() - last_refresh) >= TTL_MS then
    M.refresh()
  end
  if not M.config.next then
    return cache
  end
  local top = M.next_component()
  if cache == "" then
    return top
  end
  if top == "" then
    return cache
  end
  return string.format("%s  ▸ %s", cache, top)
end

return M
