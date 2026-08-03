-- Specs for bn.status's focus-mode behaviour (#119): inside a declared deep-work block the
-- top-action component shows the block (" focus · Nm left") in place of the next-action. We
-- stub the bn binary with a script that dispatches on the subcommand — `focus status` vs `next`
-- — so a single stub serves both calls status makes per refresh. The component is async (it
-- shells out on a throttle), so we drive the event loop with vim.wait until the cache lands.

local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

local exec = require("bn.exec")

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

-- bn.status caches the focus label and the next-action in module-level locals, so a `require`
-- shared across specs carries one test's result into the next. Reloading per test is what makes
-- the second spec's "no block is active" genuinely start from no block: without it a slow refresh
-- leaves the *previous* test's focus label sitting in the cache, and the failure then reads as a
-- wrong value rather than as the timeout it actually is.
local function fresh_status(focus_json, next_json)
  exec.setup({ cmd = stub(focus_json, next_json) })
  local status = helpers.reload("bn.status")
  status.next_refresh()
  return status
end

-- Both of status's reads (`focus status` and `next`) are subprocesses, and the component only
-- settles once both callbacks have landed. Wait for the settled value rather than a fixed sleep,
-- and say what we were waiting for when it never arrives — a timeout and a wrong value are
-- different bugs and shouldn't produce the same message.
local function await(status, want, label)
  local ok = vim.wait(2000, function()
    return status.next_component():find(want, 1, true) ~= nil
  end, 20)
  assert.is_true(ok, ("timed out waiting for the %s refresh to land"):format(label))
end

-- A next envelope with one ranked action (the "review" the next-action would otherwise show).
local NEXT = '{"ok":true,"command":"next","data":{"actions":[{"kind":"review","title":"review o/a#1"}]}}'

describe("bn.status focus mode", function()
  it("shows the focus block in place of the next-action while active", function()
    local focus = '{"ok":true,"command":"focus","data":{"active":true,'
      .. '"block":{"id":"f1","remaining_secs":2820}}}'
    local status = fresh_status(focus, NEXT)
    await(status, "focus", "focus-block")
    -- 2820s → ceil to 47m.
    assert.are.equal(" focus · 47m left", status.next_component())
  end)

  it("falls back to the next-action when no block is active", function()
    local none = '{"ok":true,"command":"focus","data":{"active":false,"block":null}}'
    local status = fresh_status(none, NEXT)
    await(status, "review", "next-action")
    assert.are.equal(" review o/a#1", status.next_component())
  end)
end)
