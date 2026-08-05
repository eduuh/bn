-- bn.hl — syntax + highlight groups for the `bn` filetype the cockpit renders into.
-- Linked to standard groups so it inherits the user's colourscheme. Idempotent.

local M = {}

function M.setup()
  -- Section count headers ("3 jobs", "12 branches", the date line) — first line of a panel.
  vim.api.nvim_set_hl(0, "bnHeader", { link = "Title", default = true })
  -- A healthy cron job / done goal.
  vim.api.nvim_set_hl(0, "bnOk", { link = "DiagnosticOk", default = true })
  -- A failed cron job.
  vim.api.nvim_set_hl(0, "bnBad", { link = "DiagnosticError", default = true })
  -- Doctor flags (stale / blocked / no_goal) and blocker counts.
  vim.api.nvim_set_hl(0, "bnWarn", { link = "DiagnosticWarn", default = true })
  -- Dividers and hints.
  vim.api.nvim_set_hl(0, "bnMuted", { link = "Comment", default = true })
  -- Numbered panel title bars (winbar): inactive vs the focused panel.
  vim.api.nvim_set_hl(0, "bnTitle", { link = "TabLine", default = true })
  vim.api.nvim_set_hl(0, "bnTitleActive", { link = "TabLineSel", default = true })
  -- Data values rendered in panel rows: counts, percentages, durations, metric numbers.
  vim.api.nvim_set_hl(0, "bnNum", { link = "Number", default = true })
  -- A repo/branch slug ("bn/feat-x") or PR tag ("repo#123") — the row's identity.
  vim.api.nvim_set_hl(0, "bnKey", { link = "Identifier", default = true })
  -- Relative times ("3h ago") and ISO dates — present, but secondary to the value.
  vim.api.nvim_set_hl(0, "bnTime", { link = "Special", default = true })

  vim.api.nvim_create_autocmd("FileType", {
    pattern = "bn",
    group = vim.api.nvim_create_augroup("bnSyntax", { clear = true }),
    callback = M.apply,
  })
end

-- Buffer-local syntax matches. Cheap: a handful of patterns over short panel buffers.
function M.apply()
  -- Defined FIRST so the glyph / warn / header matches below — defined later — win wherever
  -- they overlap a bare value (a `3 blk` count stays a warning, the line-1 count stays a header).
  local cmds = {
    -- Data values: standalone counts, percentages, and durations (`30h`, `1.5d`, `74%`).
    [[syntax match bnNum   /\<\d\+\(\.\d\+\)\?[dh]\>/]],
    [[syntax match bnNum   /\<\d\+%/]],
    [[syntax match bnNum   /\<\d\+\>/]],
    -- Row identity: a repo/branch slug (`bn/feat-x`) or a PR tag (`arena#123`).
    [[syntax match bnKey   /\<[[:alnum:]_.-]\+\/[[:alnum:]_.-]\+/]],
    [[syntax match bnKey   /\<[[:alnum:]_.-]\+#\d\+/]],
    -- Time: relative ages (`3h ago`) and ISO dates (`2026-06-26`).
    [[syntax match bnTime  /\<\d\+[mhd] ago\>/]],
    [[syntax match bnTime  /\<\d\{4}-\d\d-\d\d\>/]],
    [[syntax match bnOk    /✓/]],
    -- A logged achievement marker in the STAND panel reads as a win.
    [[syntax match bnOk    /★/]],
    -- A completed daily goal checkbox.
    [[syntax match bnOk    /\[x\]/]],
    [[syntax match bnBad   /✗/]],
    [[syntax match bnBad   /\<exit [1-9]\d*\>/]],
    -- NEXT action glyphs: review-blocked / stale-PR read as errors; raised attention warns.
    [[syntax match bnBad   //]],
    [[syntax match bnBad   //]],
    [[syntax match bnBad   //]],
    [[syntax match bnWarn  //]],
    -- A surface that failed to load (vs. a genuinely empty one).
    [[syntax match bnWarn  /⚠.*/]],
    [[syntax match bnWarn  /\<\d\+ blk\>/]],
    [[syntax match bnWarn  /\<\(stale\|blocked\|no_goal\)\(:\S*\)\?/]],
    [[syntax match bnWarn  /\<stale:\d\+d\>/]],
    -- Lower-priority NEXT glyphs (a pending goal / a blocked note) read as muted.
    [[syntax match bnMuted /◷/]],
    [[syntax match bnMuted /○/]],
    [[syntax match bnMuted /── .* ──/]],
    [[syntax match bnMuted /(press ↵ to open)/]],
    [[syntax match bnMuted /(press ↵ to open · y to yank)/]],
    [[syntax match bnMuted /(no recorded output)/]],
    -- The count header is always the panel's first visible line.
    [[syntax match bnHeader /\%1l\s*\d\+ \(job\|branch\|open\|PR\).*/]],
  }
  for _, c in ipairs(cmds) do
    pcall(vim.cmd, c)
  end
end

return M
