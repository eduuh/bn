local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

-- Specs for the BRIEF panel: the newest Chief-of-Staff card pinned above NEXT. Render is a pure
-- projection of the fetched { ref, full } (a CardRef plus its loaded body); the `m` key and
-- `available()` shell out, so those stub bn.exec directly (the panel captures the cached module).

local function chief(read)
  return {
    ref = { agent = "chief-am", date = "2026-07-07", headline = "Morning brief", read = read, path = "/x" },
    full = {
      agent = "chief-am",
      date = "2026-07-07",
      headline = "Morning brief",
      body_md = "Ship the harness today.",
      items = {
        { kind = "review", severity = "warn", title = "Review PR #40", link = "https://example.com/40" },
        { kind = "goal", severity = "info", title = "Ship harness", link = vim.NIL },
        { kind = "goal", severity = "info", title = "Write the spec", link = "https://example.com/spec" },
        { kind = "goal", severity = "info", title = "Fourth (dropped)", link = "https://example.com/4" },
      },
    },
  }
end

describe("panels.brief.render", function()
  local brief = helpers.reload("bn.panels.brief")

  it("renders a nil surface as a load-failure hint, no rows", function()
    local lines, rows = brief.render(nil)
    assert.are.same({ "  ⚠ couldn't load `bn staff feed` — is bn up to date?" }, lines)
    assert.are.same({}, rows)
  end)

  it("renders the empty state when no chief card is present", function()
    local lines, rows = brief.render({ ref = nil })
    assert.are.same({ "  (no brief today)" }, lines)
    assert.are.same({}, rows)
  end)

  it("collapses to a single dim line once the brief is read", function()
    local lines, rows = brief.render(chief(true))
    assert.are.same({ "  ✓ Morning brief  · read" }, lines)
    assert.are.equal("chief-am", rows[1].ref.agent)
  end)

  it("renders headline + top-3 items as follow-able rows when unread", function()
    local lines, rows = brief.render(chief(false))
    assert.are.same({
      "  Morning brief",
      "  1  Review PR #40",
      "  2 · Ship harness",
      "  3 · Write the spec",
    }, lines)
    -- Only three items surface; the fourth is dropped.
    assert.are.equal(4, #lines)
    -- Every row carries the card (so detail always shows the body); item rows also carry a link.
    assert.is_nil(rows[1].link)
    assert.are.equal("https://example.com/40", rows[2].link)
    assert.is_nil(rows[3].link) -- vim.NIL link coerces to nil
    assert.are.equal("https://example.com/spec", rows[4].link)
    assert.are.equal("Morning brief", rows[2].full.headline)
  end)
end)

describe("panels.brief.detail", function()
  local brief = helpers.reload("bn.panels.brief")

  it("renders the full card body from the pre-loaded card", function()
    local out
    brief.detail(chief(false), function(lines)
      out = lines
    end)
    local blob = table.concat(out, "\n")
    assert.is_truthy(blob:find("Morning brief", 1, true))
    assert.is_truthy(blob:find("chief-am · 2026-07-07", 1, true))
    assert.is_truthy(blob:find("Ship the harness today.", 1, true))
    assert.is_truthy(blob:find("Review PR #40", 1, true))
  end)
end)

describe("panels.brief keys + availability", function()
  local brief, exec
  local saved

  before_each(function()
    brief = helpers.reload("bn.panels.brief")
    exec = require("bn.exec")
    saved = { run = exec.run, json_sync_raw = exec.json_sync_raw }
  end)

  after_each(function()
    exec.run = saved.run
    exec.json_sync_raw = saved.json_sync_raw
  end)

  it("`m` marks the brief read through `bn staff feed <agent>` and refreshes", function()
    local captured, refreshed
    exec.run = function(args, cb)
      captured = args
      if cb then
        cb("")
      end
    end
    brief.keys["m"]({ refresh = function()
      refreshed = true
    end }, { ref = { agent = "chief-am", read = false } })
    assert.are.same({ "staff", "feed", "chief-am" }, captured)
    assert.is_true(refreshed)
  end)

  it("`m` is a no-op (no shell-out) when the brief is already read", function()
    local called = false
    exec.run = function()
      called = true
    end
    brief.keys["m"]({ refresh = function() end }, { ref = { agent = "chief-am", read = true } })
    assert.is_false(called)
  end)

  it("`available()` is true only when a chief card exists", function()
    exec.json_sync_raw = function()
      return { cards = { { agent = "chief-am", date = "2026-07-07", read = false } } }
    end
    assert.is_true(brief.available())

    exec.json_sync_raw = function()
      return { cards = { { agent = "mentor", date = "2026-07-07" } } }
    end
    assert.is_false(brief.available())

    exec.json_sync_raw = function()
      return nil
    end
    assert.is_false(brief.available())
  end)
end)
