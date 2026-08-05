-- Headless cockpit-open SMOKE spec.
--
-- Stubs a `bn` binary on PATH that answers each surface's `--json` with canned
-- data, opens the full cockpit (`require("bn").world`), and asserts:
--   * every panel buffer populates with its fixture's rows (not the loading or
--     load-failure line), and
--   * `q` tears the cockpit down and restores the exact tabpage/window layout
--     we had before opening it.
--
-- This is the one spec that exercises the real async fetch path end to end:
-- vim.system → envelope unwrap → repaint, across all six panels at once.

local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

-- A canned envelope per bn subcommand. The cockpit fans out next/doctor/agent/
-- standup/cron in parallel on open (STANDUP and TOMORROW share `bn standup`).
local RESPONSES = {
  next = '{"ok":true,"data":{"actions":[{"kind":"review","title":"Review PR #115","link":"https://example.com/pr/115"}]}}',
  doctor = '{"ok":true,"data":{"findings":[{"kind":"cron","job":"pr-observe","failures":1,"last_run":1750000000}]}}',
  standup = '{"ok":true,"data":{"date":"2026-06-26","yesterday":{"date":"2026-06-25","completed":[],"incomplete":[]},"today":[{"id":"g1","text":"Ship the harness","link":null,"pending_days":1}],"blockers":[],"upcoming":[{"id":"s1","text":"Review the RFC","scheduled":"2026-06-28","link":null}]}}',
  cron = '{"ok":true,"data":{"jobs":[{"job":"pr-observe","scope":"global","last_exit":0,"last_run":1750000000,"runs":3,"failures":0}]}}',
  -- `bn agent live --json` (DISPATCH panel): first non-flag arg is `agent`.
  agent = '{"ok":true,"data":{"agents":[{"repo":"bn","branch":"feat-x","target":"bn:feat-x","harness":"copilot","state":"waiting","health":"active","idle_secs":30,"has_window":true,"tokens":12300,"turns":4}],"count":1,"total_tokens":12300}}',
}

-- Build a `bn` shim on its own PATH dir that dispatches on the first non-flag arg.
local function install_bn_stub()
  local script = { "#!/usr/bin/env bash", "sub=" }
  -- First non-dash arg is the subcommand (skip the trailing --json and flags).
  table.insert(script, 'for a in "$@"; do case "$a" in --*) ;; *) sub="$a"; break;; esac; done')
  table.insert(script, 'case "$sub" in')
  for sub, json in pairs(RESPONSES) do
    table.insert(script, string.format("  %s) printf '%%s' %s ;;", sub, vim.fn.shellescape(json)))
  end
  table.insert(script, '  *) printf "%s" \'{"ok":false,"error":{"message":"unknown"}}\'; exit 1 ;;')
  table.insert(script, "esac")
  local path, dir = helpers.executable("bn", script, "cockpit")
  return dir, path
end

describe("cockpit smoke", function()
  local prev_path, stub_dir
  local bn, ui

  before_each(function()
    -- Headless nvim starts at a minimal size; give the cockpit room so its
    -- nine-way left split + detail/footer panes all get a valid window.
    vim.o.lines = 60
    vim.o.columns = 200
    stub_dir = select(1, install_bn_stub())
    prev_path = vim.env.PATH
    vim.env.PATH = stub_dir .. ":" .. prev_path
    -- Fresh modules so exec.config.cmd is nil and the `bn` on PATH is resolved.
    for _, m in ipairs({ "bn", "bn.exec", "bn.ui", "bn.dash" }) do
      package.loaded[m] = nil
    end
    bn = require("bn")
    ui = require("bn.ui")
    bn.exec.setup({ cmd = nil })
  end)

  after_each(function()
    pcall(ui.close)
    vim.env.PATH = prev_path
  end)

  -- One open per spec: the cockpit's layout builder assumes a fresh tabpage with
  -- the initial window geometry, so we exercise open → populate → q-restore in a
  -- single flow rather than reopening within the same headless session.
  it("opens, populates every panel from the stubbed bn, and restores layout on q", function()
    local before_tabs = #vim.api.nvim_list_tabpages()
    local before_tab = vim.api.nvim_get_current_tabpage()
    local before_wins = #vim.api.nvim_tabpage_list_wins(before_tab)

    bn.world()
    -- The cockpit lives on a brand-new tabpage.
    assert.are.equal(before_tabs + 1, #vim.api.nvim_list_tabpages())
    assert.are_not.equal(before_tab, vim.api.nvim_get_current_tabpage())

    -- The fan-out is async and panels land in arbitrary order, so concatenate
    -- every bn buffer and wait until each fixture's marker has been rendered (or
    -- time out). Polling the actual content avoids a race where the sweep is only
    -- partway done.
    local markers = { "Review PR #115", "failing", "bn:feat-x", "Ship the harness", "Review the RFC", "pr-observe" }
    local function blob_now()
      local all = {}
      for _, w in ipairs(vim.api.nvim_tabpage_list_wins(vim.api.nvim_get_current_tabpage())) do
        local buf = vim.api.nvim_win_get_buf(w)
        if vim.bo[buf].filetype == "bn" then
          vim.list_extend(all, vim.api.nvim_buf_get_lines(buf, 0, -1, false))
        end
      end
      return table.concat(all, "\n")
    end
    local ok = vim.wait(5000, function()
      local b = blob_now()
      for _, m in ipairs(markers) do
        if not b:find(m, 1, true) then
          return false
        end
      end
      return true
    end, 25)
    assert.is_true(ok)

    local blob = blob_now()

    -- Each fixture's distinctive content made it through fetch → unwrap → render.
    assert.is_truthy(blob:find("Review PR #115", 1, true)) -- NEXT
    assert.is_truthy(blob:find("cron pr-observe failing", 1, true)) -- ATTENTION (doctor)
    assert.is_truthy(blob:find("bn:feat-x", 1, true)) -- DISPATCH (agent live)
    assert.is_truthy(blob:find("needs you", 1, true)) -- DISPATCH state label
    assert.is_truthy(blob:find("Ship the harness", 1, true)) -- STANDUP (today)
    assert.is_truthy(blob:find("Review the RFC", 1, true)) -- TOMORROW (upcoming)
    assert.is_truthy(blob:find("pr%-observe")) -- CRON
    -- No panel fell back to its load-failure hint.
    assert.is_nil(blob:find("is bn up to date", 1, true))

    -- `q` is mapped to ui.close; invoke it the way the keymap would and confirm
    -- the cockpit tabpage is gone and we're back exactly where we started.
    ui.close()
    assert.are.equal(before_tabs, #vim.api.nvim_list_tabpages())
    assert.are.equal(before_tab, vim.api.nvim_get_current_tabpage())
    assert.are.equal(before_wins, #vim.api.nvim_tabpage_list_wins(before_tab))
  end)
end)
