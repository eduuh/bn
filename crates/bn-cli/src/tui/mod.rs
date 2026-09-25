//! The native picker: the interactive list surface `bn` draws itself, instead of shelling to fzf.
//!
//! # Why this exists
//!
//! Driving fzf means the row list, the preview and every key action have to survive a trip through
//! a shell. fzf cannot call back into a running process, so each binding re-executes `bn` with a
//! hidden `--pick-stage` flag; rows become delimiter-joined strings whose delimiters collide with
//! real branch names; and paths get shell-quoted into `--bind` arguments. Owning the frame deletes
//! all three problems at once: a binding is a closure, a row carries a typed payload, and nothing
//! is ever handed to `sh`.
//!
//! # Shape
//!
//! Everything except [`keys`] and [`term`] is pure and unit-tested without a terminal: [`model`]
//! owns what a keystroke *means*, [`render`] owns what a frame *looks like*, [`fuzzy`] owns
//! ranking, [`ansi`] owns measurement. This module is the thin driver that wires them to a tty.
//!
//! During the migration [`backend`] lets `BN_PICKER` pick which frontend runs, so a native picker
//! that misbehaves is one environment variable away from the fzf path it replaced.

pub(crate) mod ansi;
mod fuzzy;
mod keys;
mod model;
mod render;
mod term;

pub(crate) use keys::Key;

use model::{State, Step};
use std::time::Duration;

/// How long the loop waits for a key before deciding the user has paused — the window a preview is
/// computed in, so holding a key down never fires one `git` storm per repeat.
const IDLE: Duration = Duration::from_millis(40);

/// One row: what the user sees, what the matcher searches, and what the caller gets back.
pub(crate) struct Item<T> {
    pub(crate) display: String,
    pub(crate) haystack: String,
    pub(crate) key: T,
}

impl<T> Item<T> {
    /// A row whose searchable text is its display text, minus any styling.
    pub(crate) fn new(display: impl Into<String>, key: T) -> Self {
        let display = display.into();
        Self {
            haystack: ansi::strip_ansi(&display),
            display,
            key,
        }
    }

    /// Search different text than is displayed — e.g. match on a full `repo/branch` ref while the
    /// row shows a decorated, abbreviated form.
    pub(crate) fn searchable(mut self, haystack: impl Into<String>) -> Self {
        self.haystack = haystack.into();
        self
    }
}

/// Why rows are being (re)built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reload {
    /// The first build, when the picker opens.
    Initial,
    /// After an action changed the world (fzf's `execute(...)+reload(...)`).
    Soft,
    /// The user asked for fresh data, bypassing any cache (fzf's `ctrl-r`).
    Hard,
}

/// Where rows come from. `Lazy` is the native form of `--bind=start:reload(...)`: the picker paints
/// its frame first and builds rows after, so the popup is on screen immediately.
pub(crate) enum Rows<'a, T> {
    Ready(Vec<Item<T>>),
    Lazy(Box<dyn FnMut(Reload) -> Vec<Item<T>> + 'a>),
}

impl<T> Rows<'_, T> {
    fn produce(&mut self, why: Reload) -> Vec<Item<T>> {
        match self {
            Rows::Ready(items) => std::mem::take(items),
            Rows::Lazy(f) => f(why),
        }
    }

    /// A `Ready` list can only be produced once; a rebuild would empty it.
    fn rebuildable(&self) -> bool {
        matches!(self, Rows::Lazy(_))
    }
}

/// What a binding sees when it fires.
pub(crate) struct Ctx<'a, T> {
    pub(crate) focused: Option<&'a T>,
}

/// What the picker does once a binding's closure returns.
pub(crate) enum Action<'a> {
    /// Nothing more (fzf's `execute-silent` with no reload).
    Stay,
    /// Rebuild the rows (fzf's `execute(...)+reload(...)`).
    Reload,
    /// Rebuild the rows, bypassing caches.
    Refresh,
    /// Give the terminal back, run this, then resume and rebuild the rows — fzf's
    /// `execute(...)+reload(...)`, which is the only form every current binding uses. For an action
    /// that prints something the user must read, this is the one that works: a message written
    /// behind the alternate screen is a message nobody sees.
    Suspend(Box<dyn FnOnce() + 'a>),
}

/// A binding's handler: it sees the focused row and says what happens next.
type Handler<'a, T> = Box<dyn FnMut(Ctx<'_, T>) -> Action<'a> + 'a>;

/// Renders the side pane for the focused row.
type Previewer<'a, T> = Box<dyn FnMut(&T) -> String + 'a>;

/// A key the caller handles itself.
pub(crate) struct Binding<'a, T> {
    pub(crate) key: Key,
    /// How it reads in the footer, e.g. `"ctrl-x: kill"`.
    pub(crate) hint: &'static str,
    pub(crate) run: Handler<'a, T>,
}

/// A configured picker, ready to [`run`].
pub(crate) struct Picker<'a, T> {
    pub(crate) header: String,
    pub(crate) rows: Rows<'a, T>,
    pub(crate) multi: bool,
    pub(crate) preview: Option<Previewer<'a, T>>,
    /// Share of the width for the preview pane; 0 for no pane.
    pub(crate) preview_pct: u16,
    pub(crate) bindings: Vec<Binding<'a, T>>,
}

impl<'a, T> Picker<'a, T> {
    /// A plain single-select list — the shape most callers want.
    pub(crate) fn new(header: impl Into<String>, items: Vec<Item<T>>) -> Self {
        Self {
            header: header.into(),
            rows: Rows::Ready(items),
            multi: false,
            preview: None,
            preview_pct: 0,
            bindings: Vec::new(),
        }
    }

    pub(crate) fn rows(mut self, rows: Rows<'a, T>) -> Self {
        self.rows = rows;
        self
    }

    pub(crate) fn preview(mut self, pct: u16, f: impl FnMut(&T) -> String + 'a) -> Self {
        self.preview = Some(Box::new(f));
        self.preview_pct = pct;
        self
    }

    pub(crate) fn bind(
        mut self,
        key: Key,
        hint: &'static str,
        run: impl FnMut(Ctx<'_, T>) -> Action<'a> + 'a,
    ) -> Self {
        self.bindings.push(Binding {
            key,
            hint,
            run: Box::new(run),
        });
        self
    }
}

/// How a picker session ended. The three fzf exit codes callers already branch on map onto the
/// first three variants: a selection (0), a query nothing matched (1), and cancelled (130).
pub(crate) enum Outcome<T> {
    Selected { items: Vec<T>, query: String },
    Query(String),
    Cancelled,
}

/// Which frontend draws the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    /// Spawn fzf, as `bn` has always done.
    Fzf,
    /// Draw it ourselves.
    Native,
}

/// The default while the native picker is still proving itself. Flipping this constant is the whole
/// rollout: every picker reads it through [`backend`].
const DEFAULT_BACKEND: Backend = Backend::Fzf;

/// Resolve `$BN_PICKER`.
pub(crate) fn backend() -> Backend {
    parse_backend(std::env::var("BN_PICKER").ok().as_deref())
}

/// Pure so precedence is testable without touching process env.
///
/// An unrecognised value falls back to the default rather than failing: a typo in a shell profile
/// must never be the reason the `C-o` popup stops opening.
pub(crate) fn parse_backend(raw: Option<&str>) -> Backend {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("native") => Backend::Native,
        Some("fzf") => Backend::Fzf,
        _ => DEFAULT_BACKEND,
    }
}

/// Is there a terminal to draw on? Callers use this to choose their non-interactive degrade —
/// printing the plain list — instead of silently doing nothing.
pub(crate) fn interactive() -> bool {
    term::available()
}

/// Run the picker to completion, always leaving the terminal as it was found.
///
/// Returns [`Outcome::Cancelled`] without drawing anything when there is no tty, so a piped or
/// scripted `bn` never blocks waiting for keys that cannot arrive.
pub(crate) fn run<T: Clone>(mut p: Picker<'_, T>) -> Outcome<T> {
    if !term::available() {
        return Outcome::Cancelled;
    }
    let Some(mut screen) = term::Screen::enter() else {
        return Outcome::Cancelled;
    };
    let outcome = drive(&mut p, &mut screen);
    // Restore before returning, not on drop at some later scope exit: the caller's very next move
    // is often `tmux switch-client`, and that must not land on a screen about to be discarded.
    drop(screen);
    outcome
}

fn drive<T: Clone>(p: &mut Picker<'_, T>, screen: &mut term::Screen) -> Outcome<T> {
    let (mut cols, mut rows) = screen.size();
    let bound: Vec<Key> = p.bindings.iter().map(|b| b.key).collect();
    let hints = p
        .bindings
        .iter()
        .map(|b| format!("{}: {}", b.key.label(), b.hint))
        .collect::<Vec<_>>()
        .join(" · ");

    let mut state = State::new(Vec::new(), p.multi, true);
    // Paint an empty frame before building rows: on a cold cache the row build can take a moment,
    // and a popup that appears instantly and fills in reads as fast, while a blank terminal that
    // appears late reads as broken.
    paint(screen, p, &state, cols, rows, &hints, None);
    state.reload(p.rows.produce(Reload::Initial));

    let mut preview: Option<(usize, String)> = None;
    let mut dirty = true;
    loop {
        state.resize(list_rows(rows));
        if dirty {
            let shown = preview.as_ref().and_then(|(idx, text)| {
                (state.focused_index() == Some(*idx)).then_some(text.as_str())
            });
            paint(screen, p, &state, cols, rows, &hints, shown);
            dirty = false;
        }
        let Some(ev) = term::next_event(IDLE) else {
            // The user paused: this is the moment to pay for a preview.
            let focus = state.focused_index();
            let cached = preview.as_ref().map(|(idx, _)| *idx);
            if let (Some(f), Some(idx)) = (p.preview.as_mut(), focus) {
                if cached != Some(idx) {
                    preview = Some((idx, f(&state.items[idx].key)));
                    dirty = true;
                }
            }
            continue;
        };
        match ev {
            crossterm::event::Event::Resize(c, r) => {
                cols = c;
                rows = r;
                dirty = true;
            }
            crossterm::event::Event::Key(ke) => {
                let Some(key) = keys::from_crossterm(ke) else {
                    continue;
                };
                dirty = true;
                match state.step(key, &bound) {
                    Step::Consumed => {}
                    Step::Accept => {
                        let items = state
                            .selection()
                            .into_iter()
                            .map(|i| state.items[i].key.clone())
                            .collect();
                        return Outcome::Selected {
                            items,
                            query: state.query.clone(),
                        };
                    }
                    Step::AcceptQuery => return Outcome::Query(state.query.clone()),
                    Step::Cancel => return Outcome::Cancelled,
                    Step::Fire(i) => {
                        let ctx = Ctx {
                            focused: state.focused(),
                        };
                        match (p.bindings[i].run)(ctx) {
                            Action::Stay => {}
                            Action::Reload | Action::Refresh if !p.rows.rebuildable() => {}
                            Action::Reload => {
                                state.reload(p.rows.produce(Reload::Soft));
                                preview = None;
                            }
                            Action::Refresh => {
                                state.reload(p.rows.produce(Reload::Hard));
                                preview = None;
                            }
                            Action::Suspend(f) => {
                                screen.suspended(f);
                                if p.rows.rebuildable() {
                                    state.reload(p.rows.produce(Reload::Soft));
                                }
                                preview = None;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn list_rows(rows: u16) -> usize {
    (rows as usize).saturating_sub(3).max(1)
}

fn paint<T>(
    screen: &mut term::Screen,
    p: &Picker<'_, T>,
    state: &State<T>,
    cols: u16,
    rows: u16,
    hints: &str,
    preview: Option<&str>,
) {
    let visible: Vec<render::Line<'_>> = state
        .matches
        .iter()
        .enumerate()
        .skip(state.offset)
        .take(list_rows(rows))
        .map(|(pos, m)| render::Line {
            display: &state.items[m.idx].display,
            marked: state.marks.contains(&m.idx),
            focused: pos == state.cursor,
        })
        .collect();
    let view = render::View {
        cols,
        rows,
        header: &p.header,
        query: &state.query,
        matched: state.matches.len(),
        total: state.items.len(),
        list: &visible,
        preview,
        preview_pct: p.preview_pct,
        notice: state.notice.as_deref(),
        hints,
    };
    screen.paint(&render::frame(&view));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_toggle_reads_the_way_a_shell_profile_writes_it() {
        assert_eq!(parse_backend(Some("native")), Backend::Native);
        assert_eq!(parse_backend(Some("NATIVE")), Backend::Native);
        assert_eq!(parse_backend(Some(" native ")), Backend::Native);
        assert_eq!(parse_backend(Some("fzf")), Backend::Fzf);
    }

    /// A typo must not be the reason the popup stops opening.
    #[test]
    fn anything_unrecognised_falls_back_to_the_default() {
        assert_eq!(parse_backend(None), DEFAULT_BACKEND);
        assert_eq!(parse_backend(Some("")), DEFAULT_BACKEND);
        assert_eq!(parse_backend(Some("nativ")), DEFAULT_BACKEND);
    }

    #[test]
    fn an_item_searches_its_text_without_the_styling() {
        let i = Item::new("\x1b[32m●\x1b[0m feat-auth", "ref");
        assert_eq!(i.haystack, "● feat-auth");
        assert_eq!(Item::new("x", "r").searchable("other").haystack, "other");
    }
}
