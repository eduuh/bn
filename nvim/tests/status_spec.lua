-- Specs for bn.status's focus-mode behaviour (#119): inside a declared deep-work block the
-- top-action component shows the block (" focus · Nm left") in place of the next-action. We
-- stub the bn binary with a script that dispatches on the subcommand — `focus status` vs `next`
-- — so a single stub serves both calls status makes per refresh. The component is async (it
-- shells out on a throttle), so we drive the event loop with vim.wait until the cache lands.

local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

local exec = require("bn.exec")
local status = require("bn.status")

-- A bn stub that returns `focus_json` for `focus status` calls and `next_json` for `next` calls,
-- so one binary drives both reads status performs on a refresh.
local function stub(focus_json, next_json)
  return helpers.executable("bn-stub", {
    "#!/usr/bin/env bash",
    'case "$*" in',
    "  *focus*) printf '%s' " .. vim.fn.shellescape(focus_json) .. " ;;",
    "  *next*)  printf '%s' " .. vim.fn.shellescape(next_json) .. " ;;",
    "esac",
    "exit 0",
  }, "status")
end

-- A next envelope with one ranked action (the "review" the next-action would otherwise show).
local NEXT = '{"ok":true,"command":"next","data":{"actions":[{"kind":"review","title":"review o/a#1"}]}}'

describe("bn.status focus mode", function()
  it("shows the focus block in place of the next-action while active", function()
    local focus = '{"ok":true,"command":"focus","data":{"active":true,'
      .. '"block":{"id":"f1","remaining_secs":2820}}}'
    exec.setup({ cmd = stub(focus, NEXT) })
    status.next_refresh()
    -- both async reads must land before the cache reflects the block.
    vim.wait(1000, function()
      return status.next_component():find("focus") ~= nil
    end)
    -- 2820s → ceil to 47m.
    assert.are.equal(" focus · 47m left", status.next_component())
  end)

  it("falls back to the next-action when no block is active", function()
    local none = '{"ok":true,"command":"focus","data":{"active":false,"block":null}}'
    exec.setup({ cmd = stub(none, NEXT) })
    status.next_refresh()
    vim.wait(1000, function()
      return status.next_component():find("review") ~= nil
    end)
    assert.are.equal(" review o/a#1", status.next_component())
  end)
end)
