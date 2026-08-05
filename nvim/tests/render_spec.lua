local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

-- Pure-function specs for the panels' `render(data) → lines, rows`.
--
-- These need a Neovim runtime (the renderers lean on vim.NIL coercion and
-- string.format) but touch *no* async/system API — render is a pure projection
-- of fetched data onto display lines + per-line row metadata. We assert both the
-- exact lines and the row metadata that the cursor-driven detail pane keys off.
--
-- Covered: NEXT, ATTENTION, STANDUP, TOMORROW. CRON is intentionally not
-- asserted line-for-line here because its output folds os.time()-relative
-- phrasing ("3m ago"), which isn't deterministic for a fixed fixture; its
-- data-shaping is exercised indirectly by the cockpit smoke spec instead.

describe("panels.next.render", function()
  local next_panel = helpers.reload("bn.panels.next")

  it("renders a nil surface as a load-failure hint, no rows", function()
    local lines, rows = next_panel.render(nil)
    assert.are.same({ "  ⚠ couldn't load `bn next` — is bn up to date?" }, lines)
    assert.are.same({}, rows)
  end)

  it("renders an empty action list as 'desk is clear'", function()
    local lines, rows = next_panel.render({ actions = {} })
    assert.are.same({ "  ✓ nothing queued — desk is clear" }, lines)
    assert.are.same({}, rows)
  end)

  it("numbers and glyphs each action and carries link metadata", function()
    local data = {
      actions = {
        { kind = "review", title = "Review PR #40", detail = "from sam", link = "https://example.com/pr/40" },
        { kind = "goal", title = "Ship the harness", link = vim.NIL },
        { kind = "weird", title = "unknown kind" },
      },
    }
    local lines, rows = next_panel.render(data)
    assert.are.same({
      "  1  Review PR #40",
      "  2 ◷ Ship the harness",
      "  3 • unknown kind",
    }, lines)
    -- vim.NIL link coerces to nil; missing link stays nil; real link preserved.
    assert.are.equal("https://example.com/pr/40", rows[1].link)
    assert.are.equal("review", rows[1].kind)
    assert.is_nil(rows[2].link)
    assert.is_nil(rows[3].link)
  end)
end)

describe("panels.attn.render", function()
  local attn = helpers.reload("bn.panels.attn")

  it("renders the load-failure hint on nil data", function()
    local lines, rows = attn.render(nil)
    assert.are.same({ "  ⚠ couldn't load `bn doctor` — is bn up to date?" }, lines)
    assert.are.same({}, rows)
  end)

  it("renders the all-clear line when nothing is flagged", function()
    local lines, rows = attn.render({ findings = {} })
    assert.are.same({
      "  0 items need attention",
      "",
      "  ✓ all clear — nothing flagged",
    }, lines)
    assert.are.same({}, rows)
  end)

  it("glyphs and labels each finding by kind and carries the row", function()
    local data = {
      findings = {
        { kind = "note", repo = "bigmono", branch = "user-x", flags = { "stale:110d", "no_goal" }, blockers = 0, open_todos = 1, idle_days = 110, last_activity = "2026-03-10", path = "/p/note.md" },
        { kind = "cron", job = "pr-observe", failures = 2, last_run = os.time() - 60 },
        { kind = "attn", id = "a6", title = "merged worktree dirty", severity = "warn", source = "wt-cleanup", repo = "bn", branch = "x", link = vim.NIL },
      },
    }
    local lines, rows = attn.render(data)
    assert.are.equal("  3 items need attention", lines[1])
    assert.are.equal("", lines[2])
    assert.are.equal("  ○ bigmono/user-x  stale:110d no_goal", lines[3])
    assert.are.equal("  ⚠ cron pr-observe failing  (2 ✗)", lines[4])
    assert.are.equal("   merged worktree dirty", lines[5])
    assert.are.equal("note", rows[3].kind)
    assert.are.equal("/p/note.md", rows[3].path)
    assert.are.equal("cron", rows[4].kind)
    assert.are.equal("attn", rows[5].kind)
    assert.are.equal("a6", rows[5].id)
  end)

  it("uses the blocked glyph for notes with open blockers", function()
    local lines = attn.render({ findings = { { kind = "note", repo = "bn", branch = "y", flags = { "blockers:1" }, blockers = 1 } } })
    assert.are.equal("   bn/y  blockers:1", lines[3])
  end)
end)

describe("panels.standup.render", function()
  local standup = helpers.reload("bn.panels.standup")

  it("renders the load-failure hint on nil data", function()
    local lines = standup.render(nil)
    assert.are.same({ "  ⚠ couldn't load `bn standup` — is bn up to date?" }, lines)
  end)

  it("renders yesterday/today/blockers with their glyphs and row metadata", function()
    local data = {
      date = "2026-06-28",
      yesterday = {
        date = "2026-06-27",
        completed = { { id = "g0", text = "Shipped X", link = vim.NIL } },
        incomplete = { { id = "g1", text = "Set up SAW", link = vim.NIL } },
      },
      today = { { id = "g1", text = "Set up SAW", link = "https://example.com/saw" } },
      blockers = { { repo = "bn", branch = "x", text = "Need a decision" } },
      upcoming = {},
    }
    local lines, rows = standup.render(data)
    assert.are.same({
      "  Yesterday · 2026-06-27",
      "    ✓ Shipped X",
      "    ○ Set up SAW",
      "",
      "  Today",
      "    ◷ Set up SAW",
      "",
      "  Blockers",
      "    ! Need a decision",
    }, lines)
    assert.are.equal("goal", rows[2].kind)
    assert.are.equal("goal", rows[3].kind)
    assert.are.equal("goal", rows[6].kind)
    assert.are.equal("https://example.com/saw", rows[6].link)
    assert.are.equal("blocker", rows[9].kind)
    assert.are.equal("bn/x", rows[9].tag)
  end)

  it("shows the empty-day fallbacks when nothing is recorded", function()
    local lines = standup.render({ date = "2026-06-28", yesterday = { date = "2026-06-27", completed = {}, incomplete = {} }, today = {}, blockers = {} })
    assert.are.same({
      "  Yesterday · 2026-06-27",
      "    (nothing recorded)",
      "",
      "  Today",
      "    (no goals planned)",
    }, lines)
  end)
end)

describe("panels.tomorrow.render", function()
  local tomorrow = helpers.reload("bn.panels.tomorrow")

  it("renders the load-failure hint on nil data", function()
    local lines = tomorrow.render(nil)
    assert.are.same({ "  ⚠ couldn't load `bn standup` — is bn up to date?" }, lines)
  end)

  it("renders carryover goals and scheduled-ahead items with metadata", function()
    local data = {
      today = { { id = "g1", text = "Set up SAW", link = vim.NIL } },
      upcoming = { { id = "s1", text = "Review RFC", scheduled = "2026-06-30", link = "https://example.com/rfc" } },
    }
    local lines, rows = tomorrow.render(data)
    assert.are.same({
      "  Carrying forward",
      "    ○ Set up SAW",
      "",
      "  Scheduled ahead",
      "    → Review RFC  (due 2026-06-30)",
    }, lines)
    assert.are.equal("goal", rows[2].kind)
    assert.are.equal("upcoming", rows[5].kind)
    assert.are.equal("https://example.com/rfc", rows[5].link)
  end)

  it("shows the clean-slate line when nothing carries forward", function()
    local lines = tomorrow.render({ today = {}, upcoming = {} })
    assert.are.same({
      "  Carrying forward",
      "    (nothing open — clean slate)",
    }, lines)
  end)
end)

describe("panels.dispatch", function()
  local dispatch = helpers.reload("bn.panels.dispatch")

  -- Nerd Font marks (from codepoints, matching the panel) so line assertions don't hand-type
  -- private-use glyphs. Health-fallback glyphs are plain BMP literals.
  local bell = vim.fn.nr2char(0xf0f3, true) -- waiting / needs you
  local gear = vim.fn.nr2char(0xf013, true) -- working
  local check = vim.fn.nr2char(0xf00c, true) -- done

  describe("status_of", function()
    it("maps Plane-A state to glyph/label/rank, needs-you states first", function()
      assert.are.same({ glyph = bell, label = "needs you", rank = 0 }, dispatch.status_of({ state = "waiting" }))
      assert.are.same({ glyph = "○", label = "stuck", rank = 1 }, dispatch.status_of({ state = "", health = "stuck" }))
      assert.are.equal("working", dispatch.status_of({ state = "working" }).label)
      assert.are.equal(2, dispatch.status_of({ state = "working" }).rank)
      assert.are.equal("done", dispatch.status_of({ state = "done" }).label)
      assert.are.equal(3, dispatch.status_of({ state = "done" }).rank)
      assert.are.equal("blocked", dispatch.status_of({ state = "blocked" }).label)
      assert.are.equal(0, dispatch.status_of({ state = "blocked" }).rank)
    end)

    it("falls back to Plane-B health for an undecorated window, else unknown", function()
      assert.are.same({ glyph = "◐", label = "idle", rank = 4 }, dispatch.status_of({ state = "", health = "idle" }))
      assert.are.same({ glyph = "·", label = "active", rank = 5 }, dispatch.status_of({ state = "", health = "active" }))
      assert.are.same({ glyph = "·", label = "…", rank = 5 }, dispatch.status_of({}))
    end)
  end)

  it("formats compact token counts like the CLI", function()
    assert.are.equal("950", dispatch.fmt_tokens(950))
    assert.are.equal("12.3k", dispatch.fmt_tokens(12300))
    assert.are.equal("1.2M", dispatch.fmt_tokens(1250000))
  end)

  it("renders a nil surface as a load-failure hint, no rows", function()
    local lines, rows = dispatch.render(nil)
    assert.are.same({ "  ⚠ couldn't load `bn agent live` — is bn up to date?" }, lines)
    assert.are.same({}, rows)
  end)

  it("renders an empty fleet as 'nothing dispatched'", function()
    local lines, rows = dispatch.render({ agents = {} })
    assert.are.same({ "  ✓ no live agents — nothing dispatched" }, lines)
    assert.are.same({}, rows)
  end)

  it("ranks who-needs-you first, glyphs/labels each row, and carries jump metadata", function()
    local data = {
      agents = {
        { repo = "bn", branch = "a", target = "bn:a", harness = "copilot", state = "working", has_window = true },
        { repo = "bn", branch = "b", target = "bn:b", harness = "claude", state = "waiting", health = "active", idle_secs = 30, has_window = true, tokens = 12300, turns = 4 },
        { repo = "bn", branch = "c", target = "bn:c", harness = "copilot", state = "", health = "idle", has_window = false },
        { repo = "bn", branch = "d", target = "bn:d", harness = "copilot", state = "done", has_window = true },
      },
    }
    local lines, rows = dispatch.render(data)
    assert.are.same({
      "  4 agents · 1 waiting",
      "",
      "  " .. bell .. " needs you bn:b  12.3k tok",
      "  " .. gear .. " working   bn:a",
      "  " .. check .. " done      bn:d",
      "  ◐ idle      bn:c  (no window)",
    }, lines)
    -- Rows follow the display order (needs-you first): b, a, d, c.
    assert.are.equal("bn:b", rows[3].target)
    assert.are.equal("claude", rows[3].harness)
    assert.is_true(rows[3].has_window)
    assert.are.equal("bn:a", rows[4].target)
    assert.are.equal("bn:d", rows[5].target)
    assert.are.equal("bn:c", rows[6].target)
    assert.is_false(rows[6].has_window)
  end)

  it("coerces JSON-null health to nil (window-only rows) via util.val", function()
    local lines, rows = dispatch.render({
      agents = { { repo = "bn", branch = "x", target = "bn:x", state = "working", health = vim.NIL, has_window = true } },
    })
    assert.are.equal("  " .. gear .. " working   bn:x", lines[3])
    assert.is_nil(rows[3].health)
  end)
end)
