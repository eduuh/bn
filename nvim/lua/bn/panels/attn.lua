-- ATTENTION panel — the `bn doctor` attention list, most-pressing first. Folds three sources:
-- failing cron jobs (⚠), raised attn items (//·), and local notes that are stale, goal-less,
-- or blocked (/○). Where NEXT says "what to act on", this says "what's wrong right now".
--
-- Reads `bn doctor --json` → { findings: [ {kind = note|cron|attn, ...} ] }.

local exec = require("bn.exec")
local util = require("bn.util")

local M = {
  title = "ATTENTION",
  footer = "↵ open note/link · A ack item · r reload",
}

local val = util.val

-- One glyph per finding, by kind and severity, so the list reads at a glance.
local function glyph(f)
  if f.kind == "cron" then
    return "⚠"
  elseif f.kind == "attn" then
    local s = f.severity
    return s == "urgent" and "" or s == "warn" and "" or "·"
  else -- note
    return (f.blockers and f.blockers > 0) and "" or "○"
  end
end

-- A short, single-line label per finding for the list column.
local function label(f)
  if f.kind == "cron" then
    return string.format("cron %s failing  (%d ✗)", f.job, f.failures or 0)
  elseif f.kind == "attn" then
    return f.title or "(attention item)"
  else -- note
    local where = (f.repo or "?") .. "/" .. (f.branch or "?")
    local flags = table.concat(f.flags or {}, " ")
    return flags ~= "" and (where .. "  " .. flags) or where
  end
end

function M.fetch(cb)
  exec.json({ "doctor" }, cb, { silent = true })
end

function M.render(data)
  local lines, rows, add = util.panel_rows()

  if data == nil then
    add("  ⚠ couldn't load `bn doctor` — is bn up to date?")
    return lines, rows
  end

  local findings = data.findings or {}
  add(string.format("  %d item%s need attention", #findings, #findings == 1 and "" or "s"))
  add("")
  if #findings == 0 then
    add("  ✓ all clear — nothing flagged")
    return lines, rows
  end

  for _, f in ipairs(findings) do
    add("  " .. glyph(f) .. " " .. label(f), f)
  end
  return lines, rows
end

M.detail_key = function(f)
  return "attn:" .. (f.kind or "") .. ":" .. (f.id or f.job or ((f.repo or "") .. "/" .. (f.branch or "")))
end

function M.detail(f, cb)
  local out = { "", "  " .. label(f), "" }
  local function line(s)
    table.insert(out, "  " .. s)
  end
  if f.kind == "cron" then
    line("kind:      failing cron job")
    line("failures:  " .. (f.failures or 0))
    line("last run:  " .. util.ago(f.last_run))
    if val(f.repo) then
      line("scope:     " .. f.repo .. "/" .. (f.branch or "?"))
    end
  elseif f.kind == "attn" then
    line("kind:      attention item " .. (f.id or ""))
    line("severity:  " .. (f.severity or "?"))
    line("source:    " .. (f.source or "?"))
    if val(f.repo) then
      line("scope:     " .. f.repo .. "/" .. (f.branch or "?"))
    end
    if val(f.link) then
      line("link:      " .. f.link)
      line("(press ↵ to open · A to ack)")
    else
      line("(press A to ack)")
    end
  else -- note
    line("kind:      note needing attention")
    line("flags:     " .. table.concat(f.flags or {}, " "))
    line("open todos: " .. (f.open_todos or 0))
    line("blockers:  " .. (f.blockers or 0))
    line("idle:      " .. (f.idle_days or 0) .. "d  (last " .. (f.last_activity or "?") .. ")")
    if val(f.path) then
      line("(press ↵ to open the note)")
    end
  end
  cb(out)
end

M.keys = {
  ["<cr>"] = function(ctx, f)
    if not f then
      return
    end
    if f.kind == "note" and val(f.path) then
      ctx.open(f.path)
    else
      util.open_link(f.link)
    end
  end,
  ["A"] = function(ctx, f)
    if f and f.kind == "attn" and val(f.id) then
      exec.run({ "attn", "ack", f.id }, function()
        ctx.refresh()
      end)
    else
      vim.notify("bn: only attention items can be acked", vim.log.levels.INFO)
    end
  end,
  ["r"] = function(ctx)
    ctx.refresh()
  end,
}

return M
