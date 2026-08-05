-- bn.util — small helpers shared across panels: JSON-null coercion, relative-time
-- formatting, NEXT glyphs, panel rows, and link actions.

local M = {}

local NEXT_GLYPH = {
  cron = "⚠",
  attn = "",
  review = "",
  pr = "",
  goal = "◷",
  blocked = "○",
}

--- JSON nulls arrive as vim.NIL (truthy in Lua) — coerce those (and "") to nil so
--- callers can treat "absent" uniformly.
function M.val(v)
  if v == nil or v == vim.NIL or v == "" then
    return nil
  end
  return v
end

--- A glyph per `bn next` action kind, shared by the NEXT panel and statusline.
function M.next_glyph(kind)
  return NEXT_GLYPH[kind] or "•"
end

--- Create the standard panel render accumulator: display lines, row metadata, and add().
function M.panel_rows()
  local lines, rows = {}, {}
  local function add(text, meta)
    table.insert(lines, text)
    if meta then
      rows[#lines] = meta
    end
  end
  return lines, rows, add
end

--- A unix epoch as a compact "Nm/h/d ago", or "never" when absent. Mirrors the CLI.
function M.ago(epoch)
  epoch = M.val(epoch)
  if not epoch then
    return "never"
  end
  local secs = os.time() - epoch
  if secs < 3600 then
    return math.max(1, math.floor(secs / 60)) .. "m ago"
  elseif secs < 86400 then
    return math.floor(secs / 3600) .. "h ago"
  end
  return math.floor(secs / 86400) .. "d ago"
end

--- An ISO-8601 UTC timestamp ("2026-06-21T13:53:56Z") as "Nm/h/d ago". Falls back to
--- the raw string when it can't be parsed, so unexpected formats still show *something*.
function M.iso_ago(iso)
  iso = M.val(iso)
  if not iso then
    return "never"
  end
  local y, mo, d, h, mi, s = iso:match("(%d+)-(%d+)-(%d+)T(%d+):(%d+):(%d+)")
  if not y then
    return iso
  end
  -- os.time treats the fields as local; offset by the local UTC bias so the delta is honest.
  local utc = os.time({ year = y, month = mo, day = d, hour = h, min = mi, sec = s })
  local bias = os.difftime(os.time(), os.time(os.date("!*t")))
  return M.ago(utc + bias)
end

--- Open a link in the OS browser, or notify when there's nothing to open.
function M.open_link(link)
  link = M.val(link)
  if not link then
    return vim.notify("bn: no link here", vim.log.levels.INFO)
  end
  if vim.ui.open then
    vim.ui.open(link)
  else
    vim.fn.jobstart({ "xdg-open", link }, { detach = true })
  end
end

--- Copy a link to the unnamed + system clipboard registers, with a short confirmation.
function M.yank_link(link)
  link = M.val(link)
  if not link then
    return vim.notify("bn: no link to yank", vim.log.levels.INFO)
  end
  vim.fn.setreg('"', link)
  vim.fn.setreg("+", link)
  vim.notify("bn: yanked " .. link, vim.log.levels.INFO)
end

--- Standard keymap set for panels whose selected rows carry a `link` field.
function M.link_keys()
  return {
    ["<cr>"] = function(_, row)
      if row then
        M.open_link(row.link)
      end
    end,
    ["y"] = function(_, row)
      if row then
        M.yank_link(row.link)
      end
    end,
    ["r"] = function(ctx)
      ctx.refresh()
    end,
  }
end

return M
