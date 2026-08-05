local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

-- Specs for the STAFF panel: the roster + health list, its card-history detail pane, and the
-- R/P/+/-/e/m actions. Render/detail are pure projections (detail reads card files off disk, so we
-- write fixtures); the action keys shell out, so those stub bn.exec on the cached module.

local function roster()
  return {
    agents = {
      {
        name = "mentor",
        on_calendar = "*-*-* 06:30:00",
        harness = "copilot",
        scope = "global",
        sinks = {},
        last_status = "ok",
        last_ts = os.time() - 120,
        unread_card = true,
        last_error = vim.NIL,
      },
      {
        name = "scout",
        on_calendar = "Sat *-*-* 10:00:00",
        harness = "copilot",
        scope = "global",
        sinks = {},
        last_status = "fail",
        last_ts = os.time() - 3600,
        unread_card = false,
        last_error = "boom",
      },
    },
    cards = {},
  }
end

describe("panels.staff.render", function()
  local staff = helpers.reload("bn.panels.staff")

  it("renders the load-failure hint on nil data", function()
    local lines, rows = staff.render(nil)
    assert.are.same({ "  ⚠ couldn't load `bn staff` — is bn up to date?" }, lines)
    assert.are.same({}, rows)
  end)

  it("renders the empty roster note when no agents are declared", function()
    local lines = staff.render({ agents = {}, cards = {} })
    assert.are.equal("  0 agents", lines[1])
    assert.are.equal("  no staff agents declared (see config.example/agents.toml)", lines[3])
  end)

  it("rows each agent with an unread dot, schedule, and last-run health", function()
    local lines, rows = staff.render(roster())
    assert.are.equal("  2 agents", lines[1])
    -- Unread + healthy → ● dot and a ✓ health mark; failed run → ✗ lead and the status word.
    assert.is_truthy(lines[3]:find("● mentor", 1, true))
    assert.is_truthy(lines[3]:find("✓", 1, true))
    assert.is_truthy(lines[4]:find("✗ scout", 1, true))
    assert.is_truthy(lines[4]:find("fail", 1, true))
    -- The systemd OnCalendar is humanized to the mock's short form, never shown raw.
    assert.is_truthy(lines[3]:find("06:30", 1, true))
    assert.is_nil(lines[3]:find("*-*-*", 1, true))
    assert.is_truthy(lines[4]:find("Sat", 1, true))
    assert.is_nil(lines[4]:find("*-*-*", 1, true))
    -- Row metadata carries what the detail/actions need.
    assert.are.equal("mentor", rows[3].name)
    assert.are.same({}, rows[3].cards)
    assert.are.equal("scout", rows[4].name)
    assert.are.equal("boom", rows[4].last_error)
  end)

  it("humanizes systemd OnCalendar into the short cockpit schedule form", function()
    local h = staff._humanize_schedule
    assert.are.equal("06:30", h("*-*-* 06:30:00")) -- daily → time
    assert.are.equal("22:30", h("*-*-* 22:30:00"))
    assert.are.equal("Sat", h("Sat *-*-* 10:00:00")) -- weekday-scoped → weekday
    assert.are.equal("Sun", h("Sun *-*-* 18:00:00"))
    assert.are.equal("Mon..Fri", h("Mon..Fri *-*-* 09:00:00")) -- range kept
    assert.are.equal("Sat,Sun", h("Sat,Sun *-*-* 10:00:00")) -- list kept
    assert.are.equal("manual", h(nil)) -- absent → manual
    assert.are.equal("manual", h(vim.NIL))
    assert.are.equal("weird", h("weird")) -- unknown shape passes through
  end)

  it("shows the read dot (○) for an agent with no unread card", function()
    local data = roster()
    data.agents[1].unread_card = false
    local lines = staff.render(data)
    assert.is_truthy(lines[3]:find("○ mentor", 1, true))
  end)

  it("renders the #273 taxonomy: silent (quiet day) vs missed (no run) distinctly", function()
    local data = {
      agents = {
        {
          name = "mentor",
          on_calendar = "*-*-* 06:30:00",
          scope = "global",
          sinks = {},
          health = "ok-silent",
          last_status = "ok",
          last_ts = os.time() - 120,
          unread_card = false,
          last_error = vim.NIL,
        },
        {
          name = "chief",
          on_calendar = "Mon..Fri *-*-* 05:30:00",
          scope = "global",
          sinks = {},
          health = "missed",
          last_status = "ok",
          last_ts = os.time() - 90000,
          missed_since = os.time() - 3600,
          unread_card = false,
          last_error = vim.NIL,
        },
      },
      cards = {},
    }
    local lines = staff.render(data)
    -- A by-design quiet day reads "silent" with a normal (non-✗) lead — not death.
    assert.is_truthy(lines[3]:find("silent", 1, true))
    assert.is_nil(lines[3]:find("✗", 1, true))
    -- A missed scheduled run reads "missed" behind a ✗ lead — silence that is death.
    assert.is_truthy(lines[4]:find("✗ chief", 1, true))
    assert.is_truthy(lines[4]:find("missed", 1, true))
  end)
end)

describe("panels.staff.detail card history", function()
  local staff, rows, dir

  before_each(function()
    staff = helpers.reload("bn.panels.staff")
    dir = helpers.scratch_dir("staff")
    local p1 = dir .. "/new.json"
    local p2 = dir .. "/old.json"
    vim.fn.writefile({
      vim.json.encode({ agent = "mentor", date = "2026-07-07", headline = "Design lesson", body_md = "Body A", items = {} }),
    }, p1)
    vim.fn.writefile({
      vim.json.encode({ agent = "mentor", date = "2026-07-06", headline = "Older lesson", body_md = "Body B", items = {} }),
    }, p2)
    local data = roster()
    data.cards = {
      { agent = "mentor", date = "2026-07-07", headline = "Design lesson", read = false, path = p1 },
      { agent = "mentor", date = "2026-07-06", headline = "Older lesson", read = true, path = p2 },
    }
    _, rows = staff.render(data)
  end)

  it("shows the latest card with a history position header", function()
    staff._page["mentor"] = 1
    local out
    staff.detail(rows[3], function(lines)
      out = table.concat(lines, "\n")
    end)
    assert.is_truthy(out:find("card 1/2 · 2026-07-07", 1, true))
    assert.is_truthy(out:find("Design lesson", 1, true))
    assert.is_truthy(out:find("Body A", 1, true))
    assert.is_truthy(out:find("schedule: 06:30", 1, true))
  end)

  it("pages back through history with the page index", function()
    staff._page["mentor"] = 2
    local out
    staff.detail(rows[3], function(lines)
      out = table.concat(lines, "\n")
    end)
    assert.is_truthy(out:find("card 2/2 · 2026-07-06", 1, true))
    assert.is_truthy(out:find("Body B", 1, true))
  end)

  it("clamps the page index to the available history", function()
    staff._page["mentor"] = 99
    staff.detail(rows[3], function() end)
    assert.are.equal(2, staff._page["mentor"]) -- clamped to the oldest card
  end)

  it("`[` steps older and `]` steps newer, refreshing the pane", function()
    staff._page["mentor"] = 1
    local refreshed = 0
    local ctx = { refresh = function()
      refreshed = refreshed + 1
    end }
    staff.keys["["](ctx, rows[3]) -- older
    assert.are.equal(2, staff._page["mentor"])
    staff.keys["]"](ctx, rows[3]) -- newer
    assert.are.equal(1, staff._page["mentor"])
    assert.are.equal(2, refreshed)
  end)

  it("notes when an agent has no cards yet", function()
    local out
    staff.detail(rows[4], function(lines) -- scout has no cards
      out = table.concat(lines, "\n")
    end)
    assert.is_truthy(out:find("no cards yet", 1, true))
  end)
end)

describe("panels.staff action keys", function()
  local staff, exec, rows
  local saved

  before_each(function()
    staff = helpers.reload("bn.panels.staff")
    exec = require("bn.exec")
    saved = { run = exec.run, json = exec.json, json_raw = exec.json_raw, json_sync_raw = exec.json_sync_raw }
    _, rows = staff.render(roster())
    staff._show_plan = nil
    staff._plan = {}
  end)

  after_each(function()
    exec.run, exec.json, exec.json_raw, exec.json_sync_raw =
      saved.run, saved.json, saved.json_raw, saved.json_sync_raw
  end)

  it("`R` runs the agent now via `bn staff run <agent>`", function()
    local captured
    exec.run = function(args, cb)
      captured = args
      if cb then
        cb("")
      end
    end
    staff.keys["R"]({ refresh = function() end }, rows[3])
    assert.are.same({ "staff", "run", "mentor" }, captured)
  end)

  it("`m` marks the agent's cards read via `bn staff feed <agent>`", function()
    local captured
    exec.run = function(args, cb)
      captured = args
      if cb then
        cb("")
      end
    end
    staff.keys["m"]({ refresh = function() end }, rows[3])
    assert.are.same({ "staff", "feed", "mentor" }, captured)
  end)

  it("`P` fetches the dry-run plan and shows it in the detail pane", function()
    exec.json_raw = function(args, cb)
      assert.are.same({ "staff", "plan", "mentor" }, args)
      cb({
        gather = "bn staff gather mentor",
        skill = "staff/mentor",
        skill_found = true,
        harness = "copilot",
        sinks = { "attn", "daily" },
        packet = { foo = 1 },
      })
    end
    staff.keys["P"]({ refresh = function() end }, rows[3])
    assert.are.equal("mentor", staff._show_plan)

    local out
    staff.detail(rows[3], function(lines)
      out = table.concat(lines, "\n")
    end)
    assert.is_truthy(out:find("plan (dry-run", 1, true))
    assert.is_truthy(out:find("skill:   staff/mentor", 1, true))
    assert.is_truthy(out:find("attn, daily", 1, true))
  end)

  it("`e` resolves the agent's skill file and opens it", function()
    exec.json_raw = function(args, cb)
      cb({ skill = "staff/mentor" }) -- from `bn staff plan`
    end
    exec.json = function(args, cb)
      assert.are.same({ "skills", "list" }, args)
      cb({ skills = { { name = "staff/mentor", path = "/skills/staff/mentor.md" } } })
    end
    local opened
    staff.keys["e"]({ open = function(p)
      opened = p
    end }, rows[3])
    assert.are.equal("/skills/staff/mentor.md", opened)
  end)

  -- The `+`/`-` reactions (#268) rate the card currently shown for the agent, via
  -- `bn feed rate <date>/<agent> up|down`. A carded roster is rendered inline so a row carries the
  -- card history the rating targets.
  local function carded_rows()
    local data = roster()
    data.cards = {
      { agent = "mentor", date = "2026-07-07", headline = "New", read = false, path = "/f/new.json" },
      { agent = "mentor", date = "2026-07-06", headline = "Old", read = true, path = "/f/old.json" },
    }
    local _, r = staff.render(data)
    return r
  end

  it("`+` up-rates the latest card via `bn feed rate <date>/<agent> up`", function()
    local r = carded_rows()
    staff._page["mentor"] = 1
    local captured
    exec.run = function(args, cb)
      captured = args
      if cb then
        cb("")
      end
    end
    staff.keys["+"]({ refresh = function() end }, r[3])
    assert.are.same({ "feed", "rate", "2026-07-07/mentor", "up" }, captured)
  end)

  it("`-` down-rates the card paged to, honoring `[`/`]` history", function()
    local r = carded_rows()
    staff._page["mentor"] = 2 -- paged back to the older card
    local captured
    exec.run = function(args, cb)
      captured = args
      if cb then
        cb("")
      end
    end
    staff.keys["-"]({ refresh = function() end }, r[3])
    assert.are.same({ "feed", "rate", "2026-07-06/mentor", "down" }, captured)
  end)

  it("rating an agent with no card is a no-op that warns", function()
    local ran = false
    exec.run = function()
      ran = true
    end
    local notified
    local saved_notify = vim.notify
    vim.notify = function(msg)
      notified = msg
    end
    staff.keys["+"]({ refresh = function() end }, rows[4]) -- scout has no cards
    vim.notify = saved_notify
    assert.is_false(ran)
    assert.is_truthy(notified and notified:find("no card to rate", 1, true))
  end)

  it("`available()` is true when agents or cards exist, false otherwise", function()
    exec.json_sync_raw = function()
      return { agents = { { name = "mentor" } } }
    end
    assert.is_true(staff.available())

    -- No agents, but a card exists (second probe hits staff feed).
    local calls = 0
    exec.json_sync_raw = function(args)
      calls = calls + 1
      if args[#args] == "feed" or args[2] == "feed" then
        return { cards = { { agent = "mentor", date = "2026-07-07" } } }
      end
      return { agents = {} }
    end
    assert.is_true(staff.available())

    exec.json_sync_raw = function()
      return { agents = {}, cards = {} }
    end
    assert.is_false(staff.available())
  end)
end)
