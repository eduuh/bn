-- bn.ui — the lazybn cockpit: a lazygit-style, full-screen view of your bn world.
--
-- Layout (its own tabpage, so closing it restores exactly what you had):
--
--   ┌────────────────┬───────────────────────────┐
--   │ NEXT           │                           │
--   ├────────────────┤          DETAIL           │  ← reflects the row under the
--   │ STANDUP        │   (the selected item's    │     cursor in the focused panel
--   ├────────────────┤    expanded view)         │
--   │ CRON           │                           │
--   ├────────────────┴───────────────────────────┤
--   │ footer: contextual keys for the panel       │
--   └─────────────────────────────────────────────┘
--
-- Panels are plain tables (see panels/*.lua) with a uniform shape:
--   title   string
--   fetch(cb)            → cb(data)            async read from the bn engine
--   render(data)         → lines, rows         rows[i] = metadata for display line i
--   detail(row, cb)      → cb(lines)           the detail pane for a selected row
--   keys                 { lhs = fn(ctx, row) } contextual actions on the focused row
--   footer               string                hint line shown while focused
--
-- `ctx` handed to actions exposes { refresh(), refresh_all(), close(), open(path,line) }
-- so a panel can mutate via bn and re-read itself without knowing about windows.

local M = {}

local S = nil -- the single live cockpit, or nil when closed

-- Opt-in low-frequency background poll, in milliseconds. 0 (the default) disables it: the board
-- still refreshes on focus and on `r`. Set `require("bn.ui").poll_interval = 30000` to enable.
-- It reuses the same async fetch path, so it's just a periodic `reload_all`.
M.poll_interval = 0

-- A scratch buffer that nothing tries to write to disk.
local function scratch()
  local b = vim.api.nvim_create_buf(false, true)
  vim.bo[b].bufhidden = "wipe"
  vim.bo[b].filetype = "bn"
  return b
end

local function set_lines(buf, lines)
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
end

-- A compact "Ns/Nm/Nh ago" for the footer staleness line. Mirrors the engine's own
-- relative-time phrasing so the board reads the same as `bn`'s output.
local function ago(secs)
  if secs < 1 then
    return "just now"
  elseif secs < 60 then
    return string.format("%ds ago", secs)
  elseif secs < 3600 then
    return string.format("%dm ago", math.floor(secs / 60))
  else
    return string.format("%dh ago", math.floor(secs / 3600))
  end
end

-- A lazygit-style numbered title bar per panel ("1  DAILY"), so jumping by number is
-- discoverable; the active panel's bar is highlighted. Footer keeps the keys at the bottom.
local function paint_focus()
  for i, p in ipairs(S.panels) do
    if vim.api.nvim_win_is_valid(p.win) then
      local active = i == S.focus
      vim.wo[p.win].cursorline = active
      vim.wo[p.win].winhighlight = active and "Normal:Normal" or "Normal:NormalNC"
      local grp = active and "bnTitleActive" or "bnTitle"
      vim.wo[p.win].winbar = string.format("%%#%s# %d  %s %%*", grp, i, p.title or "")
    end
  end
  local nav = string.format("1-%d jump · ⇥ cycle · / filter · ? help · q quit", #S.panels)
  -- Right-aligned staleness clock so you always know the board's age. "refreshing…" while a
  -- reload is in flight; "updated Ns ago" once it lands.
  local age
  if S.refreshing then
    age = "refreshing…"
  elseif S.last_refresh then
    age = "updated " .. ago(os.time() - S.last_refresh)
  else
    age = ""
  end
  -- Line 1 shows the focused panel's keys, or its live filter when one is set.
  local p = S.panels[S.focus]
  local left = p.filter and ("/" .. p.filter .. "  (esc clears)") or (p.footer or "")
  set_lines(S.footer_buf, {
    "  " .. left,
    "  " .. nav .. (age ~= "" and ("    " .. age) or ""),
  })
end

-- A stable identity for the selected row, so we don't re-render (or re-shell-out) when the
-- cursor moves but the selection hasn't changed.
local function selection_key(p, meta)
  if not meta then
    return "none"
  end
  if p.detail_key then
    return p.detail_key(meta)
  end
  return tostring(meta)
end

-- Re-read the detail pane for whatever row the focused panel's cursor sits on. Skips work
-- when the selection is unchanged (unless `force`d after a data reload). Panels may call the
-- callback more than once (e.g. instant vitals, then async output) for a responsive feel.
local function refresh_detail(force)
  local p = S.panels[S.focus]
  if not (p and vim.api.nvim_win_is_valid(p.win)) then
    return
  end
  local row = vim.api.nvim_win_get_cursor(p.win)[1]
  local meta = (p.rows or {})[row]
  local key = S.focus .. ":" .. selection_key(p, meta)
  if not force and key == S.detail_sel then
    return
  end
  S.detail_sel = key
  S.detail_token = (S.detail_token or 0) + 1
  local token = S.detail_token
  local function show(lines)
    if S and S.detail_token == token and vim.api.nvim_buf_is_valid(S.detail_buf) then
      set_lines(S.detail_buf, lines)
    end
  end
  if not meta then
    return show({ "", "  (nothing selected)" })
  end
  if p.detail then
    p.detail(meta, show)
  else
    show({ "" })
  end
end

local function debounced_detail()
  if S.detail_timer then
    S.detail_timer:stop()
  end
  -- Short: dedup + per-panel caching keep this cheap, so navigation stays snappy.
  S.detail_timer = vim.defer_fn(function()
    refresh_detail(false)
  end, 25)
end

-- Render a panel's last-fetched data into its buffer, applying any live `/` filter, and keep
-- the cursor on a real row. Split out from `reload` so `/` can re-filter cheaply without
-- re-shelling-out — it just re-renders the data already in hand.
local function repaint(p)
  if not (S and vim.api.nvim_win_is_valid(p.win)) then
    return
  end
  local lines, rows = p.render(p.data)
  -- A `/` filter keeps only rows whose display line matches (case-insensitive substring),
  -- preserving the header line (rows[i] == nil). Purely a view over the fetched data.
  if p.filter and p.filter ~= "" then
    local pat = p.filter:lower()
    local fl, fr = {}, {}
    for i, line in ipairs(lines) do
      if not rows[i] then
        fl[#fl + 1] = line
        fr[#fl] = nil
      elseif line:lower():find(pat, 1, true) then
        fl[#fl + 1] = line
        fr[#fl] = rows[i]
      end
    end
    lines, rows = fl, fr
  end
  p.rows = rows
  set_lines(p.buf, lines)
  local last = math.max(#lines, 1)
  local cur = vim.api.nvim_win_get_cursor(p.win)[1]
  -- Land the cursor on a real row, not the count header, so detail has something to show.
  if not rows[cur] then
    local first
    for ln = 1, last do
      if rows[ln] then
        first = ln
        break
      end
    end
    vim.api.nvim_win_set_cursor(p.win, { math.min(first or cur, last), 0 })
  elseif cur > last then
    vim.api.nvim_win_set_cursor(p.win, { last, 0 })
  end
  if p == S.panels[S.focus] then
    refresh_detail(true)
  end
end

-- Re-fetch one panel and repaint its lines, keeping the cursor in range.
local function reload(p)
  p.fetch(function(data)
    if not (S and vim.api.nvim_win_is_valid(p.win)) then
      return
    end
    p.data = data
    repaint(p)
  end)
end

local function reload_all()
  if not S then
    return
  end
  S.refreshing = true
  S.pending = #S.panels
  paint_focus()
  for _, p in ipairs(S.panels) do
    p.fetch(function(data)
      if not (S and vim.api.nvim_win_is_valid(p.win)) then
        return
      end
      p.data = data
      repaint(p)
      -- Stamp the board's age only once the whole sweep has landed.
      S.pending = (S.pending or 1) - 1
      if S.pending <= 0 then
        S.refreshing = false
        S.last_refresh = os.time()
        paint_focus()
      end
    end)
  end
end

function M.close()
  if not S then
    return
  end
  if S.detail_timer then
    S.detail_timer:stop()
  end
  -- Stop the libuv timers (footer clock + optional poll) before tearing the tab down.
  for _, t in ipairs({ S.tick_timer, S.poll_timer }) do
    if t then
      t:stop()
      t:close()
    end
  end
  if S.help_win and vim.api.nvim_win_is_valid(S.help_win) then
    vim.api.nvim_win_close(S.help_win, true)
  end
  local tab = S.tab
  S = nil
  if tab and vim.api.nvim_tabpage_is_valid(tab) then
    -- Closing the tab's last window drops the tabpage and returns us where we were.
    vim.cmd("tabclose " .. vim.api.nvim_tabpage_get_number(tab))
  end
end

-- The context object handed to panel actions.
local function make_ctx(panel)
  return {
    refresh = function()
      reload(panel)
    end,
    refresh_all = reload_all,
    close = M.close,
    -- Close the cockpit, then open a file in the editor you came back to.
    open = function(path, line)
      M.close()
      if path and path ~= "" then
        vim.cmd.edit(vim.fn.fnameescape(path))
        if line then
          pcall(vim.api.nvim_win_set_cursor, 0, { line, 0 })
        end
      end
    end,
  }
end

-- Scroll the detail pane without leaving the focused list panel. The detail window holds long
-- content (note previews, cron output) the cursor-driven left panels can't reach otherwise.
local function scroll_detail(keys)
  if not (S and vim.api.nvim_win_is_valid(S.detail_win)) then
    return
  end
  vim.api.nvim_win_call(S.detail_win, function()
    vim.cmd("normal! " .. vim.api.nvim_replace_termcodes(keys, true, false, true))
  end)
end

-- A floating cheatsheet: global keys plus every panel's contextual keys. Toggles on `?`,
-- closes on `?`/`q`/`<Esc>`. Built fresh each open so it always reflects the live panel set.
local function toggle_help()
  if S.help_win and vim.api.nvim_win_is_valid(S.help_win) then
    vim.api.nvim_win_close(S.help_win, true)
    S.help_win = nil
    return
  end
  local lines = {
    "",
    "  lazybn — keys",
    "",
    "  global",
    string.format("    1-%-3d  jump to panel        ⇥ / ⇧⇥  cycle panels", #S.panels),
    "    j / k  move in panel        ^d / ^u  scroll detail",
    "    /      filter panel         esc      clear filter / close",
    "    r      refresh all          q        close cockpit",
    "",
  }
  for i, p in ipairs(S.panels) do
    table.insert(lines, string.format("  %d  %s", i, p.title or ""))
    table.insert(lines, "    " .. (p.footer or ""))
  end
  table.insert(lines, "")
  table.insert(lines, "  press ? / q / esc to close")

  local width = 0
  for _, l in ipairs(lines) do
    width = math.max(width, vim.fn.strdisplaywidth(l))
  end
  width = width + 2
  local height = #lines
  local buf = scratch()
  set_lines(buf, lines)
  S.help_win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    width = width,
    height = height,
    row = math.max(0, math.floor((vim.o.lines - height) / 2)),
    col = math.max(0, math.floor((vim.o.columns - width) / 2)),
    style = "minimal",
    border = "rounded",
    title = " help ",
  })
  vim.wo[S.help_win].winhighlight = "Normal:Normal,FloatBorder:bnTitle"
  for _, lhs in ipairs({ "?", "q", "<esc>" }) do
    vim.keymap.set("n", lhs, function()
      if S and S.help_win and vim.api.nvim_win_is_valid(S.help_win) then
        vim.api.nvim_win_close(S.help_win, true)
        S.help_win = nil
      end
    end, { buffer = buf, nowait = true, silent = true })
  end
end

-- lazygit-style `/`: prompt for a substring, filter the focused panel's rows to matches.
-- Re-renders from cached data (no re-fetch); an empty prompt clears the filter.
local function start_filter()
  local p = S.panels[S.focus]
  vim.ui.input({ prompt = "/" }, function(input)
    if input == nil or not (S and vim.api.nvim_win_is_valid(p.win)) then
      return
    end
    p.filter = input ~= "" and input or nil
    repaint(p)
    paint_focus()
  end)
end

-- `<Esc>` clears a live filter on the focused panel first; only closes the cockpit when there
-- is nothing to clear, so filtering never traps you.
local function clear_or_close()
  local p = S and S.panels[S.focus]
  if p and p.filter then
    p.filter = nil
    repaint(p)
    paint_focus()
    return
  end
  M.close()
end

local function focus(i)
  S.focus = ((i - 1) % #S.panels) + 1
  local p = S.panels[S.focus]
  if vim.api.nvim_win_is_valid(p.win) then
    vim.api.nvim_set_current_win(p.win)
  end
  paint_focus()
  refresh_detail(true)
end

-- Wire every key a panel buffer responds to: global navigation + the panel's own actions.
local function wire_keys(p, idx)
  local function map(lhs, fn)
    vim.keymap.set("n", lhs, fn, { buffer = p.buf, nowait = true, silent = true })
  end
  map("<tab>", function()
    focus(S.focus + 1)
  end)
  map("<s-tab>", function()
    focus(S.focus - 1)
  end)
  for n = 1, #S.panels do
    map(tostring(n), function()
      focus(n)
    end)
  end
  map("r", reload_all)
  map("/", start_filter)
  map("?", toggle_help)
  -- Scroll the detail pane from within the list (it isn't focusable by cursor).
  map("<c-d>", function()
    scroll_detail("<C-d>")
  end)
  map("<c-u>", function()
    scroll_detail("<C-u>")
  end)
  map("q", M.close)
  map("<esc>", clear_or_close)
  -- The row under the cursor, resolved fresh at press time.
  local function row()
    return (p.rows or {})[vim.api.nvim_win_get_cursor(p.win)[1]]
  end
  for lhs, fn in pairs(p.keys or {}) do
    map(lhs, function()
      fn(make_ctx(p), row())
    end)
  end
  -- Detail follows the cursor while this panel is focused.
  vim.api.nvim_create_autocmd("CursorMoved", {
    buffer = p.buf,
    callback = function()
      if S and S.panels[S.focus] == p then
        debounced_detail()
      end
    end,
  })
  -- Clicking/entering a panel makes it the focused one.
  vim.api.nvim_create_autocmd("WinEnter", {
    buffer = p.buf,
    callback = function()
      if S and S.focus ~= idx then
        S.focus = idx
        paint_focus()
        refresh_detail(true)
      end
    end,
  })
end

local function tune_win(win)
  vim.wo[win].number = false
  vim.wo[win].relativenumber = false
  vim.wo[win].signcolumn = "no"
  vim.wo[win].wrap = false
  vim.wo[win].foldenable = false
end

-- Build the tabpage layout and hand back the panel/detail/footer window handles.
local function build_layout(n_panels)
  vim.cmd("tabnew")
  local tab = vim.api.nvim_get_current_tabpage()
  vim.bo.bufhidden = "wipe"

  -- Far-right detail pane, full height.
  vim.cmd("botright vsplit")
  local detail_win = vim.api.nvim_get_current_win()

  -- Full-width footer at the very bottom: per-panel keys on line 1, global nav on line 2.
  vim.cmd("botright split")
  local footer_win = vim.api.nvim_get_current_win()
  vim.api.nvim_win_set_height(footer_win, 2)

  -- Back to the left column; stack the panels there.
  local first_left = vim.fn.win_getid(1, tab)
  vim.api.nvim_set_current_win(first_left)
  local panel_wins = { vim.api.nvim_get_current_win() }
  for _ = 2, n_panels do
    vim.cmd("belowright split")
    table.insert(panel_wins, vim.api.nvim_get_current_win())
  end

  return tab, panel_wins, detail_win, footer_win
end

-- (Re)apply the proportional layout: detail ~58% wide, panels share the left height evenly.
-- Called on open and on VimResized so the cockpit reflows with the terminal.
local function apply_sizes()
  if not (S and vim.api.nvim_win_is_valid(S.detail_win)) then
    return
  end
  pcall(vim.api.nvim_win_set_width, S.detail_win, math.floor(vim.o.columns * 0.58))
  if vim.api.nvim_win_is_valid(S.footer_win) then
    pcall(vim.api.nvim_win_set_height, S.footer_win, 2)
  end
  local left_h = vim.o.lines - 3 -- minus 2-line footer + cmdline
  local n = #S.panels
  for _, p in ipairs(S.panels) do
    if vim.api.nvim_win_is_valid(p.win) then
      pcall(vim.api.nvim_win_set_height, p.win, math.floor(left_h / n))
    end
  end
end

--- Open the cockpit with an ordered list of panel modules. `opts.focus` (a panel title) lands the
--- cursor on that panel instead of the first — so a caller can open straight to DISPATCH.
function M.open(panels, opts)
  if S then
    -- Already open: just surface it.
    if vim.api.nvim_tabpage_is_valid(S.tab) then
      vim.api.nvim_set_current_tabpage(S.tab)
      return
    end
    S = nil
  end

  local tab, wins, detail_win, footer_win = build_layout(#panels)
  S = {
    tab = tab,
    panels = {},
    detail_win = detail_win,
    detail_buf = scratch(),
    footer_win = footer_win,
    footer_buf = scratch(),
    focus = 1,
  }

  vim.api.nvim_win_set_buf(detail_win, S.detail_buf)
  vim.api.nvim_win_set_buf(footer_win, S.footer_buf)
  tune_win(detail_win)
  tune_win(footer_win)
  vim.wo[detail_win].winbar = "%#bnTitle# DETAIL %*"
  set_lines(S.detail_buf, { "", "  loading…" })

  for i, mod in ipairs(panels) do
    local p = vim.tbl_extend("force", {}, mod)
    p.win = wins[i]
    p.buf = scratch()
    vim.api.nvim_win_set_buf(p.win, p.buf)
    tune_win(p.win)
    set_lines(p.buf, { "", "  loading " .. (p.title or "") .. "…" })
    S.panels[i] = p
  end

  for i, p in ipairs(S.panels) do
    wire_keys(p, i)
  end

  apply_sizes()
  local grp = vim.api.nvim_create_augroup("bnCockpit", { clear = true })
  -- Reflow with the terminal; the autocmd self-removes once the cockpit closes.
  vim.api.nvim_create_autocmd("VimResized", {
    group = grp,
    callback = function()
      if S then
        apply_sizes()
      else
        return true
      end
    end,
  })
  -- Live cockpit: switching back to its tabpage re-reads every panel, so a cron job that just
  -- finished or a PR that just merged shows without a manual `r`.
  vim.api.nvim_create_autocmd("TabEnter", {
    group = grp,
    callback = function()
      if not S then
        return true
      end
      if vim.api.nvim_get_current_tabpage() == S.tab then
        reload_all()
      end
    end,
  })

  -- A 1s libuv ticker keeps the footer's "updated Ns ago" honest while the board sits open.
  -- It only repaints the footer text — no fetch — so it's effectively free.
  S.tick_timer = vim.uv.new_timer()
  S.tick_timer:start(
    1000,
    1000,
    vim.schedule_wrap(function()
      if S and not S.refreshing then
        paint_focus()
      end
    end)
  )
  -- Opt-in background poll (off unless poll_interval > 0), reusing the async fetch path.
  if M.poll_interval and M.poll_interval > 0 then
    S.poll_timer = vim.uv.new_timer()
    S.poll_timer:start(
      M.poll_interval,
      M.poll_interval,
      vim.schedule_wrap(function()
        if S and not S.refreshing then
          reload_all()
        end
      end)
    )
  end

  -- Land on the requested panel (by title), else the first.
  local start = 1
  if opts and opts.focus then
    for i, p in ipairs(S.panels) do
      if p.title == opts.focus then
        start = i
        break
      end
    end
  end
  focus(start)
  reload_all()
end

return M
