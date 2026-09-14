//! The subject an attention item is dispatched with (#460).

use super::model::AttnItem;
use crate::orchestrate::derive_id;

/// The dispatch subject for an attn item: its link when that resolves to an issue/PR number (so the
/// agent gets the issue directly), else the item's title as free text.
pub fn dispatch_subject(item: &AttnItem) -> String {
    if let Some(link) = &item.link {
        if derive_id(link).chars().all(|c| c.is_ascii_digit()) {
            return link.clone();
        }
    }
    item.title.clone()
}
