-- Specs for bn.exec's envelope-unwrap — the one place the `{ ok, command, data,
-- error }` contract is decoded. We stub the bn binary with a one-line script
-- (via exec.setup{ cmd = ... }) that prints canned JSON, then assert that:
--   * a healthy envelope yields `data`,
--   * a process error (non-zero exit) yields nil,
--   * an `ok:false` envelope yields nil (and surfaces the error message).
-- json_sync is blocking so it asserts directly; json is async so we drive the
-- event loop with vim.wait until the callback fires.

local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

local exec = require("bn.exec")

-- Write an executable stub that emits `stdout` and exits `code`, ignoring its
-- args. Returned path is used as exec.config.cmd, so the real `bn` is never hit.
local function stub(stdout, code)
  local lines = { "#!/usr/bin/env bash" }
  -- printf %s keeps the JSON byte-exact (no trailing newline surprises).
  table.insert(lines, "printf '%s' " .. vim.fn.shellescape(stdout))
  table.insert(lines, "exit " .. (code or 0))
  return helpers.executable("bn-stub", lines, "exec")
end

describe("exec.json_sync envelope unwrap", function()
  it("returns the envelope's data on a healthy ok:true response", function()
    exec.setup({ cmd = stub('{"ok":true,"command":"next","data":{"actions":[1,2]}}') })
    local data = exec.json_sync({ "next" })
    assert.is_not_nil(data)
    assert.are.same({ 1, 2 }, data.actions)
  end)

  it("returns nil when the process exits non-zero", function()
    exec.setup({ cmd = stub("boom", 1) })
    assert.is_nil(exec.json_sync({ "next" }))
  end)

  it("returns nil on an ok:false envelope", function()
    exec.setup({ cmd = stub('{"ok":false,"error":{"message":"no such note"}}') })
    assert.is_nil(exec.json_sync({ "next" }))
  end)

  it("returns nil on unparseable output", function()
    exec.setup({ cmd = stub("not json at all") })
    assert.is_nil(exec.json_sync({ "next" }))
  end)
end)

describe("exec.json (async) envelope unwrap", function()
  -- Drive the libuv loop until the async callback lands; capture both the
  -- argument and whether it fired at all.
  local function run(args, opts)
    local fired, result = false, "unset"
    exec.json(args, function(data)
      fired, result = true, data
    end, opts)
    vim.wait(2000, function()
      return fired
    end, 20)
    return fired, result
  end

  it("hands data to the callback on ok:true", function()
    exec.setup({ cmd = stub('{"ok":true,"command":"daily","data":{"open":3}}') })
    local fired, data = run({ "daily" })
    assert.is_true(fired)
    assert.are.equal(3, data.open)
  end)

  it("calls back with nil on a process error (still resolves, never hangs)", function()
    exec.setup({ cmd = stub("nope", 2) })
    local fired, data = run({ "daily" }, { silent = true })
    assert.is_true(fired)
    assert.is_nil(data)
  end)

  it("calls back with nil on an ok:false envelope", function()
    exec.setup({ cmd = stub('{"ok":false,"error":{"message":"stale"}}') })
    local fired, data = run({ "daily" }, { silent = true })
    assert.is_true(fired)
    assert.is_nil(data)
  end)
end)

describe("exec.json_raw (bare-payload reader)", function()
  -- Drive the libuv loop until the async callback lands (mirrors the json helper above).
  local function run(args, opts)
    local fired, result = false, "unset"
    exec.json_raw(args, function(data)
      fired, result = true, data
    end, opts)
    vim.wait(2000, function()
      return fired
    end, 20)
    return fired, result
  end

  it("hands back the WHOLE decoded object for a bare payload (no envelope)", function()
    exec.setup({ cmd = stub('{"agents":[{"name":"mentor"}]}') })
    local fired, data = run({ "staff" })
    assert.is_true(fired)
    assert.are.equal("mentor", data.agents[1].name)
  end)

  it("still unwraps `.data` when a surface does use the envelope", function()
    exec.setup({ cmd = stub('{"ok":true,"data":{"cards":[1]}}') })
    local _, data = run({ "staff", "feed" })
    assert.are.same({ 1 }, data.cards)
  end)

  it("drops non-zero-exit output by default", function()
    exec.setup({ cmd = stub('{"skill":"staff/mentor"}', 1) })
    local fired, data = run({ "staff", "plan", "mentor" }, { silent = true })
    assert.is_true(fired)
    assert.is_nil(data)
  end)

  it("parses non-zero-exit JSON when accept_nonzero_json is set (broken gather still yields skill)", function()
    exec.setup({ cmd = stub('{"skill":"staff/mentor","gather_error":"boom","packet":null}', 1) })
    local _, data = run({ "staff", "plan", "mentor" }, { silent = true, accept_nonzero_json = true })
    assert.is_not_nil(data)
    assert.are.equal("staff/mentor", data.skill)
    assert.are.equal("boom", data.gather_error)
  end)

  it("fails (nil) on non-zero exit with unparseable output even when accepting json", function()
    exec.setup({ cmd = stub("not json", 1) })
    local _, data = run({ "staff", "plan", "x" }, { silent = true, accept_nonzero_json = true })
    assert.is_nil(data)
  end)
end)
