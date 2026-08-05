-- CRON panel — every scheduled/scripted bn job across your repos, with its health, and
-- (in the detail pane) the information that job last gathered. This is how a job like
-- "report bugs created today" surfaces its output without leaving the editor.
--
-- Reads `bn cron --json` for the list and `bn cron show <job> --json` for the last run.

local exec = require("bn.exec")
local util = require("bn.util")

local M = {
  title = "CRON",
  footer = "↵/e edit script · R run now · x unregister",
}

local val = util.val

function M.fetch(cb)
  exec.json({ "cron" }, cb, { silent = true })
end

-- One glyph that captures the job's state at a glance.
local function glyph(j)
  if j.ok == true then
    return "✓"
  elseif j.ok == false then
    return "✗"
  end
  return "·" -- scripted/scheduled but never run
end

function M.render(data)
  local lines, rows, add = util.panel_rows()

  if data == nil then
    add("  ⚠ couldn't load `bn cron` — is bn up to date?")
    return lines, rows
  end

  local jobs = data.jobs or {}
  add(string.format("  %d job%s", #jobs, #jobs == 1 and "" or "s"))
  add("")
  if #jobs == 0 then
    add("  no cron jobs registered")
    return lines, rows
  end

  for _, j in ipairs(jobs) do
    local sched = j.scheduled and (val(j.schedule) or val(j.scheduler) or "scheduled") or "manual"
    add(string.format("  %s %-20s %-7s %s", glyph(j), j.job, j.scope or "?", sched), j)
  end
  return lines, rows
end

M.detail_key = function(job)
  return "cron:" .. job.job
end

-- `bn cron show` output, cached per job+run so re-visiting a job is instant. The key folds
-- in last_run, so a fresh run (which bumps last_run) busts the cache automatically.
local output_cache = {}

-- The vitals we already have from the list — no subprocess needed, so the pane fills the
-- instant you land on a job.
local function vitals(job)
  local out = { "  " .. job.job, "" }
  local function line(s)
    table.insert(out, "  " .. s)
  end
  line("scope:     " .. (job.scope or "?"))
  line("schedule:  " .. (job.scheduled and (val(job.schedule) or "(scheduled)") or "manual"))
  line("last run:  " .. util.ago(job.last_run) .. (val(job.last_exit) ~= nil and ("  (exit " .. job.last_exit .. ")") or ""))
  line(string.format("runs:      %s  ·  %s failures", job.runs or 0, job.failures or 0))
  line("script:    " .. (val(job.script_path) or "—"))
  table.insert(out, "")
  table.insert(out, "  ── last output ──")
  return out
end

-- Detail = vitals (instant) + the last run's gathered output (cached, else fetched).
function M.detail(job, cb)
  local key = job.job .. "@" .. tostring(val(job.last_run) or 0)
  local function paint(tail)
    local out = vitals(job)
    if tail and tail ~= "" then
      for _, l in ipairs(vim.split(tail, "\n", { plain = true })) do
        table.insert(out, "  " .. l)
      end
    else
      table.insert(out, "  " .. (tail == nil and "loading…" or "(no recorded output)"))
    end
    cb(out)
  end

  local cached = output_cache[key]
  if cached ~= nil then
    return paint(cached)
  end
  paint(nil) -- instant vitals while the output loads
  exec.json({ "cron", "show", job.job }, function(d)
    local run = d and d.run
    local tail = (run and val(run.output_tail)) or ""
    output_cache[key] = tail
    paint(tail)
  end, { silent = true })
end

local function edit_script(ctx, job)
  local script_path = val(job.script_path)
  if script_path then
    ctx.open(script_path)
  else
    vim.notify("bn: '" .. job.job .. "' has no script to edit", vim.log.levels.INFO)
  end
end

M.keys = {
  ["e"] = function(ctx, row)
    if row then
      edit_script(ctx, row)
    end
  end,
  ["<cr>"] = function(ctx, row)
    if row then
      edit_script(ctx, row)
    end
  end,
  ["R"] = function(ctx, row)
    if row then
      vim.notify("bn: running " .. row.job .. "…", vim.log.levels.INFO)
      exec.run({ "run", row.job }, function()
        ctx.refresh()
      end)
    end
  end,
  ["x"] = function(ctx, row)
    if not row then
      return
    end
    local ok = vim.fn.confirm("Unregister cron '" .. row.job .. "'?", "&Yes\n&No", 2)
    if ok == 1 then
      exec.run({ "cron", "unregister", row.job }, function()
        ctx.refresh()
      end)
    end
  end,
}

return M
