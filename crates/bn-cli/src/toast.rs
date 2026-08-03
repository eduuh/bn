//! The CLI's one-liner into the notification policy gate ([`bn_core::notify::policy`]).
//!
//! The raw toast backend is `pub(crate)` inside `bn-core`, so every desktop notification in the
//! CLI comes through here. A call site's whole job is to declare *intent* — how loud this is
//! ([`Class`]) and the stable identity of the condition (the key) — and the gate decides whether
//! it is worth interrupting for. See the module docs on [`bn_core::notify::policy`] for why.
//!
//! Keys name the **condition**, not the observation: `orchestrate:bn/feat-x:question`, not
//! `orchestrate:bn/feat-x:question:<timestamp>`. A key that embeds the moment can never dedup,
//! which is the bug that had a parked lane re-toasting every fifteen seconds.

pub(crate) use bn_core::notify::policy::Class;
use bn_core::notify::policy::{deliver, Notification};

/// Offer one notification to the gate. Returns whether it actually reached the screen — almost no
/// caller cares (a toast is best-effort), so most sites discard it.
pub(crate) fn toast(class: Class, key: &str, source: &str, title: &str, body: &str) -> bool {
    let n = Notification::new(class, key, source, title, body);
    deliver(&n, bn_core::util::now_epoch_secs()).sent()
}

/// [`toast`] for a notification a human explicitly asked for with a flag — see
/// [`bn_core::notify::policy::Notification::user_requested`].
pub(crate) fn toast_requested(
    class: Class,
    key: &str,
    source: &str,
    title: &str,
    body: &str,
) -> bool {
    let n = Notification::new(class, key, source, title, body).requested();
    deliver(&n, bn_core::util::now_epoch_secs()).sent()
}
