//! The interaction model: every keystroke's effect on the picker, with no terminal attached.
//!
//! This module never touches crossterm, stdout or a tty, which is the point — the whole behaviour
//! of the picker (filtering, cursor movement, marking, what Enter means when nothing matched) is
//! exercised in tests by feeding a `Vec<Key>` and reading the state back.

use super::fuzzy::{Match, Ranker};
use super::keys::Key;
use super::Item;
use std::collections::BTreeSet;

/// What the driver should do about a key the model has already applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Handled internally — repaint and keep going.
    Consumed,
    /// Run the caller's binding at this index.
    Fire(usize),
    /// Close with the focused row (or the marks, in multi mode).
    Accept,
    /// Close with the typed query — nothing matched it.
    AcceptQuery,
    /// Close with nothing.
    Cancel,
}

pub(crate) struct State<T> {
    pub(crate) items: Vec<Item<T>>,
    pub(crate) matches: Vec<Match>,
    pub(crate) query: String,
    /// Index into `matches`.
    pub(crate) cursor: usize,
    /// Index into `matches` of the first visible row.
    pub(crate) offset: usize,
    /// Indices into `items` — a mark rides the row, not its current position, so it survives a
    /// query change that reshuffles or hides it and comes back when the query widens again.
    pub(crate) marks: BTreeSet<usize>,
    pub(crate) notice: Option<String>,
    pub(crate) multi: bool,
    /// How many rows the list pane can show; the driver keeps this in step with the terminal.
    pub(crate) page: usize,
    ranker: Ranker,
}

impl<T> State<T> {
    pub(crate) fn new(items: Vec<Item<T>>, multi: bool, paths: bool) -> Self {
        let mut s = Self {
            items,
            matches: Vec::new(),
            query: String::new(),
            cursor: 0,
            offset: 0,
            marks: BTreeSet::new(),
            notice: None,
            multi,
            page: 10,
            ranker: Ranker::new(paths),
        };
        s.refilter();
        s
    }

    /// Swap in a fresh row set (a reload), keeping the query and doing its best to keep the cursor
    /// on the same row.
    pub(crate) fn reload(&mut self, items: Vec<Item<T>>) {
        // Follow the row by what it *says*, not by where it sat: a reload re-derives the list from
        // the world, so an action that removed a row above the cursor shifts every index below it.
        let focused = self.focused_index().map(|i| self.items[i].display.clone());
        self.items = items;
        self.marks.clear();
        self.refilter();
        if let Some(display) = focused {
            if let Some(idx) = self.items.iter().position(|i| i.display == display) {
                if let Some(pos) = self.matches.iter().position(|m| m.idx == idx) {
                    self.cursor = pos;
                }
            }
        }
        self.clamp();
    }

    pub(crate) fn refilter(&mut self) {
        let haystacks: Vec<String> = self.items.iter().map(|i| i.haystack.clone()).collect();
        self.matches = self.ranker.rank(&self.query, &haystacks);
        self.clamp();
    }

    /// The `items` index under the cursor.
    pub(crate) fn focused_index(&self) -> Option<usize> {
        self.matches.get(self.cursor).map(|m| m.idx)
    }

    pub(crate) fn focused(&self) -> Option<&T> {
        self.focused_index().map(|i| &self.items[i].key)
    }

    /// Marked rows, or the focused row when nothing is marked — fzf's `-m` behaviour.
    pub(crate) fn selection(&self) -> Vec<usize> {
        if self.multi && !self.marks.is_empty() {
            return self.marks.iter().copied().collect();
        }
        self.focused_index().into_iter().collect()
    }

    pub(crate) fn resize(&mut self, page: usize) {
        self.page = page.max(1);
        self.clamp();
    }

    fn clamp(&mut self) {
        if self.matches.is_empty() {
            self.cursor = 0;
            self.offset = 0;
            return;
        }
        self.cursor = self.cursor.min(self.matches.len() - 1);
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        let last_visible = self.offset + self.page.saturating_sub(1);
        if self.cursor > last_visible {
            self.offset = self.cursor + 1 - self.page;
        }
        let max_offset = self.matches.len().saturating_sub(self.page);
        self.offset = self.offset.min(max_offset);
    }

    fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() as isize - 1;
        let next = (self.cursor as isize + delta).clamp(0, last);
        self.cursor = next as usize;
        self.clamp();
    }

    /// Apply one key. `bound` is the caller's binding keys, which take precedence over every
    /// built-in — including plain characters, so a `m`-to-mark-read binding shadows typing `m`,
    /// the same resolution fzf makes.
    pub(crate) fn step(&mut self, k: Key, bound: &[Key]) -> Step {
        self.notice = None;
        if let Some(i) = bound.iter().position(|b| *b == k) {
            return Step::Fire(i);
        }
        match k {
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('g') => Step::Cancel,
            Key::Enter => {
                if self.matches.is_empty() {
                    Step::AcceptQuery
                } else {
                    Step::Accept
                }
            }
            Key::Tab if self.multi => {
                if let Some(idx) = self.focused_index() {
                    if !self.marks.insert(idx) {
                        self.marks.remove(&idx);
                    }
                }
                self.move_by(1);
                Step::Consumed
            }
            Key::Up | Key::Ctrl('p') => {
                self.move_by(-1);
                Step::Consumed
            }
            Key::Down | Key::Ctrl('n') => {
                self.move_by(1);
                Step::Consumed
            }
            Key::PageUp => {
                self.move_by(-(self.page as isize));
                Step::Consumed
            }
            Key::PageDown => {
                self.move_by(self.page as isize);
                Step::Consumed
            }
            Key::Home => {
                self.cursor = 0;
                self.clamp();
                Step::Consumed
            }
            Key::End => {
                self.cursor = self.matches.len().saturating_sub(1);
                self.clamp();
                Step::Consumed
            }
            Key::Backspace => {
                self.query.pop();
                self.refilter();
                Step::Consumed
            }
            Key::Ctrl('u') => {
                self.query.clear();
                self.refilter();
                Step::Consumed
            }
            Key::Ctrl('w') => {
                let trimmed = self.query.trim_end();
                let cut = trimmed.rfind([' ', '/', '-', '_']).map_or(0, |i| i + 1);
                self.query.truncate(cut);
                self.refilter();
                Step::Consumed
            }
            Key::Char(c) => {
                self.query.push(c);
                self.refilter();
                Step::Consumed
            }
            _ => Step::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(rows: &[&str]) -> State<String> {
        let items = rows
            .iter()
            .map(|r| Item::new((*r).to_string(), (*r).to_string()))
            .collect();
        let mut s = State::new(items, false, true);
        s.resize(3);
        s
    }

    fn press(s: &mut State<String>, keys: &[Key]) -> Step {
        let mut last = Step::Consumed;
        for k in keys {
            last = s.step(*k, &[]);
        }
        last
    }

    fn visible(s: &State<String>) -> Vec<String> {
        s.matches
            .iter()
            .map(|m| s.items[m.idx].key.clone())
            .collect()
    }

    #[test]
    fn typing_filters_and_backspace_restores() {
        let mut s = state(&["alpha", "beta", "gamma"]);
        press(&mut s, &[Key::Char('b'), Key::Char('e')]);
        assert_eq!(visible(&s), vec!["beta"]);
        press(&mut s, &[Key::Backspace, Key::Backspace]);
        assert_eq!(visible(&s), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn ctrl_u_clears_the_whole_query() {
        let mut s = state(&["alpha", "beta"]);
        press(&mut s, &[Key::Char('b'), Key::Ctrl('u')]);
        assert!(s.query.is_empty());
        assert_eq!(visible(&s).len(), 2);
    }

    #[test]
    fn the_cursor_clamps_at_both_ends() {
        let mut s = state(&["a", "b"]);
        press(&mut s, &[Key::Up, Key::Up]);
        assert_eq!(s.cursor, 0);
        press(&mut s, &[Key::Down, Key::Down, Key::Down]);
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn the_viewport_follows_the_cursor() {
        let mut s = state(&["a", "b", "c", "d", "e"]);
        assert_eq!(s.offset, 0);
        press(&mut s, &[Key::Down, Key::Down, Key::Down]);
        assert_eq!(s.cursor, 3);
        assert!(
            s.offset > 0,
            "offset must scroll to keep the cursor visible"
        );
        assert!(s.cursor < s.offset + s.page);
    }

    #[test]
    fn enter_accepts_a_match_but_yields_the_query_when_nothing_matched() {
        let mut s = state(&["alpha"]);
        assert_eq!(press(&mut s, &[Key::Enter]), Step::Accept);
        press(&mut s, &[Key::Char('z'), Key::Char('z')]);
        assert_eq!(press(&mut s, &[Key::Enter]), Step::AcceptQuery);
    }

    #[test]
    fn esc_and_ctrl_c_both_cancel() {
        let mut s = state(&["a"]);
        assert_eq!(press(&mut s, &[Key::Esc]), Step::Cancel);
        assert_eq!(press(&mut s, &[Key::Ctrl('c')]), Step::Cancel);
    }

    #[test]
    fn a_bound_key_fires_instead_of_being_typed() {
        let mut s = state(&["a"]);
        assert_eq!(s.step(Key::Ctrl('x'), &[Key::Ctrl('x')]), Step::Fire(0));
        // A bare character binding shadows the query editor, the way fzf resolves it.
        assert_eq!(
            s.step(Key::Char('m'), &[Key::Ctrl('x'), Key::Char('m')]),
            Step::Fire(1)
        );
        assert!(s.query.is_empty());
    }

    #[test]
    fn tab_marks_in_multi_mode_and_marks_ride_the_row_not_the_position() {
        let items = ["alpha", "beta", "gamma"]
            .iter()
            .map(|r| Item::new((*r).to_string(), (*r).to_string()))
            .collect();
        let mut s = State::new(items, true, true);
        s.resize(5);
        s.step(Key::Tab, &[]); // mark "alpha", cursor moves to "beta"
        s.step(Key::Tab, &[]); // mark "beta"
        assert_eq!(s.selection().len(), 2);
        // Narrow so the marked rows are filtered out, then widen again.
        press(&mut s, &[Key::Char('g')]);
        press(&mut s, &[Key::Backspace]);
        assert_eq!(
            s.selection().len(),
            2,
            "marks must survive a query round-trip"
        );
    }

    #[test]
    fn without_marks_multi_select_falls_back_to_the_focused_row() {
        let items = ["alpha", "beta"]
            .iter()
            .map(|r| Item::new((*r).to_string(), (*r).to_string()))
            .collect();
        let s = State::new(items, true, true);
        assert_eq!(s.selection().len(), 1);
    }

    #[test]
    fn a_reload_keeps_the_cursor_on_the_same_row() {
        let mut s = state(&["a", "b", "c"]);
        press(&mut s, &[Key::Down]);
        assert_eq!(s.focused().map(String::as_str), Some("b"));
        let items = ["z", "a", "b", "c"]
            .iter()
            .map(|r| Item::new((*r).to_string(), (*r).to_string()))
            .collect();
        s.reload(items);
        assert_eq!(s.focused().map(String::as_str), Some("b"));
    }

    #[test]
    fn an_empty_list_does_not_panic() {
        let mut s = state(&[]);
        press(&mut s, &[Key::Down, Key::Up, Key::PageDown, Key::End]);
        assert!(s.focused().is_none());
        assert_eq!(press(&mut s, &[Key::Enter]), Step::AcceptQuery);
    }
}
