local helpers = dofile(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h") .. "/helpers.lua")
helpers.prepend_runtimepath()

-- Pure-function specs for bn.panels.card — the shared helpers the BRIEF and STAFF panels use to
-- pick, read, and render the staff framework's briefing cards.

describe("panels.card helpers", function()
  local card = helpers.reload("bn.panels.card")

  it("recognises chief-am / chief-pm as the Chief of Staff", function()
    assert.is_true(card.is_chief("chief-am"))
    assert.is_true(card.is_chief("chief-pm"))
    assert.is_false(card.is_chief("mentor"))
    -- Exact match only — a coincidentally-named agent isn't the Chief.
    assert.is_false(card.is_chief("chief-reviewer"))
    assert.is_false(card.is_chief("chiefly"))
    assert.is_false(card.is_chief(vim.NIL))
    assert.is_false(card.is_chief(nil))
  end)

  it("picks the newest chief card, preferring unread on a date tie", function()
    local cards = {
      { agent = "mentor", date = "2026-07-07", read = false, headline = "not chief" },
      { agent = "chief-am", date = "2026-07-06", read = true, headline = "old brief" },
      { agent = "chief-pm", date = "2026-07-07", read = true, headline = "read today" },
      { agent = "chief-am", date = "2026-07-07", read = false, headline = "fresh today" },
    }
    local best = card.newest_chief(cards)
    assert.are.equal("fresh today", best.headline)
    -- No chief cards → nil.
    assert.is_nil(card.newest_chief({ { agent = "mentor", date = "2026-07-07" } }))
    assert.is_nil(card.newest_chief({}))
  end)

  it("prefers the PM slot over AM on the same day (evening beats morning)", function()
    local best = card.newest_chief({
      { agent = "chief-am", date = "2026-07-07", read = false, headline = "morning" },
      { agent = "chief-pm", date = "2026-07-07", read = false, headline = "evening" },
    })
    assert.are.equal("evening", best.headline)
  end)

  it("an unread AM still beats a read PM on the same day", function()
    local best = card.newest_chief({
      { agent = "chief-pm", date = "2026-07-07", read = true, headline = "read pm" },
      { agent = "chief-am", date = "2026-07-07", read = false, headline = "unread am" },
    })
    assert.are.equal("unread am", best.headline)
  end)

  it("groups an agent's cards newest-first", function()
    local cards = {
      { agent = "mentor", date = "2026-07-05" },
      { agent = "scout", date = "2026-07-07" },
      { agent = "mentor", date = "2026-07-07" },
      { agent = "mentor", date = "2026-07-06" },
    }
    local mentor = card.agent_cards(cards, "mentor")
    assert.are.equal(3, #mentor)
    assert.are.equal("2026-07-07", mentor[1].date)
    assert.are.equal("2026-07-06", mentor[2].date)
    assert.are.equal("2026-07-05", mentor[3].date)
    assert.are.same({}, card.agent_cards(cards, "nobody"))
  end)

  it("reads a card JSON file without touching it (no read-mark)", function()
    local dir = helpers.scratch_dir("card")
    local path = dir .. "/card.json"
    vim.fn.writefile({
      vim.json.encode({ agent = "mentor", date = "2026-07-07", headline = "H", body_md = "B", items = {} }),
    }, path)
    local c = card.read_card_file(path)
    assert.are.equal("mentor", c.agent)
    assert.are.equal("B", c.body_md)
    -- Missing / nil paths degrade to nil, never an error.
    assert.is_nil(card.read_card_file(dir .. "/does-not-exist.json"))
    assert.is_nil(card.read_card_file(vim.NIL))
    assert.is_nil(card.read_card_file(nil))
  end)

  it("renders a card into detail lines: headline, byline, body, items", function()
    local lines = card.detail_lines({
      agent = "chief-am",
      date = "2026-07-07",
      headline = "Morning brief",
      body_md = "Line one\nLine two",
      items = {
        { kind = "review", severity = "warn", title = "Review PR #40", link = "https://e/40" },
        { kind = "goal", severity = "info", title = "Ship harness", link = "" },
      },
    })
    assert.are.same({
      "",
      "  Morning brief",
      "  chief-am · 2026-07-07",
      "",
      "  Line one",
      "  Line two",
      "",
      "  ── items ──",
      "  • [warn] review: Review PR #40  (https://e/40)",
      "  • [info] goal: Ship harness",
    }, lines)
  end)

  it("renders an unavailable card as a placeholder, never an error", function()
    assert.are.same({ "", "  (card unavailable)" }, card.detail_lines(nil))
  end)
end)
