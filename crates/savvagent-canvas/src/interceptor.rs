//! Renderer-side default-action interceptor. Runs AFTER raw event
//! dispatch (`events::dispatch_raw`). Inspects the targeted DOM node;
//! if it matches a default-action element (link, summary, form button),
//! produces an `Effect` for the host to apply.
//!
//! Why renderer-side: keeps Blitz's headless eventing self-contained.
//! Effects flow up via `InputOutcome::effects` so the host still
//! mediates the actual shell-out.

#![warn(missing_docs)]

use blitz_dom::{BaseDocument, ElementData, local_name, qual_name};
use savvagent_plugin::{Effect, UrlTarget};

/// Examine the node at `target_node`; if it triggers a default
/// action, return the `Effect` to apply. Returns `None` for
/// non-default-action targets.
///
/// This is the read-only path: it handles default actions that do
/// not mutate the DOM (link follow → `Effect::OpenUrl`). DOM-mutating
/// default actions (`<details>` toggle in Task 11, form submit in
/// Task 12) will land on a sibling `intercept_mut` that takes
/// `&mut BaseDocument`; the tag dispatch below grows `summary` and
/// `button`/`input` arms then.
//
// `#[allow(dead_code)]`: `intercept` is wired into `HtmlCanvas::dispatch`
// in Task 13. Until then only the pure `classify_url` path (and its
// tests) reference this module's symbols. Remove the allow when Task 13
// lands.
#[allow(dead_code)]
pub fn intercept(base: &BaseDocument, target_node: Option<u32>) -> Option<Effect> {
    let id = target_node?;
    let node = base.get_node(id as usize)?;
    let element = node.data.downcast_element()?;
    // Mirror focus.rs's local-name comparison idiom (`*local == *"a"`).
    let local = &element.name.local;
    if *local == *"a" {
        return link_effect(element);
    }
    // `summary` (Task 11) and form `button`/`input` (Task 12) arms are
    // added here, routed through a mutating `intercept_mut`. Everything
    // else is not a default-action target.
    None
}

/// Result of a mutating interception pass.
//
// `#[allow(dead_code)]`: the `effect`/`dirty` fields are read by
// `HtmlCanvas::dispatch` in Task 13 (it surfaces the effect and
// re-resolves when `dirty`). Until then only this module's tests read
// them, and tests don't count toward dead-code analysis. Remove the
// allow when Task 13 lands.
#[allow(dead_code)]
#[derive(Debug)]
pub struct InterceptOutcome {
    /// Effect to surface to the host, or `None` for internal-only mutations.
    pub effect: Option<Effect>,
    /// True if the DOM was mutated and the caller must re-resolve.
    pub dirty: bool,
}

/// Like [`intercept`] but may mutate the DOM. Currently handles the
/// `<details>` toggle (clicking a `<summary>` flips its parent
/// `<details>`'s `open` attribute) and delegates `<a>` to the
/// read-only [`intercept`] path. Everything else is a no-op.
//
// `#[allow(dead_code)]`: `intercept_mut` is wired into
// `HtmlCanvas::dispatch` (which re-resolves when `dirty`) in Task 13.
// Until then only its own tests exercise it. Remove the allow when
// Task 13 lands.
#[allow(dead_code)]
pub fn intercept_mut(base: &mut BaseDocument, target_node: Option<u32>) -> InterceptOutcome {
    let id = match target_node {
        Some(id) => id,
        None => {
            return InterceptOutcome {
                effect: None,
                dirty: false,
            };
        }
    };
    // Read the tag (immutable) before any mutation; the local name is
    // an `Atom`/interned string, so clone it to an owned `String` to
    // drop the borrow on `base` before re-borrowing mutably below.
    let tag = base
        .get_node(id as usize)
        .and_then(|n| n.data.downcast_element())
        .map(|e| e.name.local.to_string());
    match tag.as_deref() {
        Some("a") => InterceptOutcome {
            // The link path doesn't mutate; reuse the Task 10 logic.
            effect: intercept(base, Some(id)),
            dirty: false,
        },
        Some("summary") => toggle_details_parent(base, id),
        _ => InterceptOutcome {
            effect: None,
            dirty: false,
        },
    }
}

/// Flip the `open` attribute on the `<details>` parent of the clicked
/// `<summary>`. No-op (non-dirty) if the summary has no parent or its
/// parent isn't a `<details>`.
fn toggle_details_parent(base: &mut BaseDocument, summary_id: u32) -> InterceptOutcome {
    let no_op = InterceptOutcome {
        effect: None,
        dirty: false,
    };

    // 1. Find the parent node id and confirm it's a `<details>` with /
    //    without `open` — all reads happen up front so the immutable
    //    borrow is released before we take the mutator.
    let parent_id = match base.get_node(summary_id as usize).and_then(|n| n.parent) {
        Some(p) => p,
        None => return no_op,
    };
    let currently_open = match base
        .get_node(parent_id)
        .and_then(|n| n.data.downcast_element())
    {
        Some(e) if *e.name.local == *"details" => e.attr(local_name!("open")).is_some(),
        // Parent isn't a <details>: leave the DOM alone.
        _ => return no_op,
    };

    // 2. Toggle via the document mutator. `set_attribute`/`clear_attribute`
    //    snapshot the node and mark restyle damage internally, so the
    //    caller only needs to re-resolve. The mutator flushes on drop.
    let open = qual_name!("open");
    let mut mutator = base.mutate();
    if currently_open {
        mutator.clear_attribute(parent_id, open);
    } else {
        // `<details open>` is a boolean attribute; presence is what
        // matters, so an empty value is the canonical form.
        mutator.set_attribute(parent_id, open, "");
    }
    drop(mutator);

    InterceptOutcome {
        effect: None,
        dirty: true,
    }
}

/// Map an `<a href>` element to an `Effect::OpenUrl`, classifying the
/// href's scheme. Returns `None` if the anchor has no `href` or the
/// scheme is one we deliberately drop (see [`classify_url`]).
fn link_effect(element: &ElementData) -> Option<Effect> {
    let href = element.attr(local_name!("href"))?.to_string();
    let target = classify_url(&href)?;
    Some(Effect::OpenUrl { url: href, target })
}

/// Classify an href per the Phase 2 spec's URL-scheme table.
pub fn classify_url(href: &str) -> Option<UrlTarget> {
    let lower = href.to_ascii_lowercase();
    // http(s) and the messaging schemes (mailto/tel/sms) all hand off to
    // the system browser/handler; grouped into one arm so clippy doesn't
    // flag the (intentionally) identical bodies.
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("sms:")
    {
        Some(UrlTarget::SystemBrowser)
    } else if lower.starts_with("data:") {
        tracing::debug!(href, "interceptor: data: URL ignored");
        None
    } else if lower.starts_with("javascript:") {
        tracing::warn!(href, "interceptor: javascript: URL blocked");
        None
    } else if lower.starts_with("file://") {
        tracing::debug!(href, "interceptor: file:// URL ignored (subset violation)");
        None
    } else if href.contains("://") {
        tracing::warn!(href, "interceptor: unknown URL scheme; no effect emitted");
        None
    } else {
        Some(UrlTarget::ContinueConversation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_dom::BaseDocument;

    /// Depth-first search for the first element whose local tag name
    /// matches `tag`, returning its node id. Mirrors `focus.rs`'s walk:
    /// Blitz node ids and `node.children` entries are `usize` slab keys;
    /// we cast to `u32` only at the boundary the interceptor expects.
    /// `events.rs` has its own copy in a sibling test module; the two
    /// are independent because Rust test modules don't share helpers.
    fn find_node_by_tag(base: &BaseDocument, tag: &str) -> Option<u32> {
        fn walk(base: &BaseDocument, id: usize, tag: &str) -> Option<usize> {
            let node = base.get_node(id)?;
            if let Some(e) = node.data.downcast_element()
                && *e.name.local == *tag
            {
                return Some(id);
            }
            for c in node.children.iter().copied() {
                if let Some(found) = walk(base, c, tag) {
                    return Some(found);
                }
            }
            None
        }
        walk(base, base.root_element().id, tag).map(|id| id as u32)
    }

    #[test]
    fn summary_click_returns_redraw_signal() {
        let html = "<!doctype html><body><details><summary>s</summary><p>body</p></details></body>";
        let mut doc = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: None,
                net_provider: None,
                style_threading: blitz_dom::StyleThreading::Sequential,
                viewport: Some(blitz_traits::shell::Viewport::new(
                    800,
                    600,
                    1.0,
                    blitz_traits::shell::ColorScheme::Light,
                )),
                ..Default::default()
            },
        );
        {
            let base: &mut BaseDocument = doc.as_mut();
            base.resolve(0.0);
        }
        let summary_id = {
            let base: &BaseDocument = doc.as_ref();
            find_node_by_tag(base, "summary").expect("summary present")
        };
        let details_id_before = {
            let base: &BaseDocument = doc.as_ref();
            let summary = base.get_node(summary_id as usize).unwrap();
            let parent = summary.parent.expect("summary has parent");
            let details = base.get_node(parent).unwrap();
            let element = details.data.downcast_element().unwrap();
            assert!(
                element.attr(blitz_dom::local_name!("open")).is_none(),
                "details should start closed"
            );
            parent as u32
        };
        let base: &mut BaseDocument = doc.as_mut();
        let outcome = crate::interceptor::intercept_mut(base, Some(summary_id));
        assert!(outcome.dirty, "summary click should mutate DOM");
        assert!(
            outcome.effect.is_none(),
            "no Effect for summary click — internal-only"
        );
        let details = base.get_node(details_id_before as usize).unwrap();
        let element = details.data.downcast_element().unwrap();
        assert!(
            element.attr(blitz_dom::local_name!("open")).is_some(),
            "details should now be open"
        );
    }

    #[test]
    fn https_routes_to_system_browser() {
        assert_eq!(classify_url("https://example.com"), Some(UrlTarget::SystemBrowser));
        assert_eq!(classify_url("HTTP://Example.com/path"), Some(UrlTarget::SystemBrowser));
    }
    #[test]
    fn mailto_routes_to_system_browser() {
        assert_eq!(classify_url("mailto:user@example.com"), Some(UrlTarget::SystemBrowser));
    }
    #[test]
    fn tel_and_sms_route_to_system_browser() {
        assert_eq!(classify_url("tel:+15551234567"), Some(UrlTarget::SystemBrowser));
        assert_eq!(classify_url("sms:+15551234567"), Some(UrlTarget::SystemBrowser));
    }
    #[test]
    fn data_url_emits_no_effect() {
        assert_eq!(classify_url("data:text/plain,hello"), None);
    }
    #[test]
    fn javascript_url_is_blocked() {
        assert_eq!(classify_url("javascript:alert(1)"), None);
        assert_eq!(classify_url("JAVASCRIPT:alert(1)"), None);
    }
    #[test]
    fn file_url_emits_no_effect() {
        assert_eq!(classify_url("file:///etc/passwd"), None);
    }
    #[test]
    fn unknown_scheme_emits_no_effect() {
        assert_eq!(classify_url("steam://run/440"), None);
    }
    #[test]
    fn bare_path_continues_conversation() {
        assert_eq!(classify_url("./foo.md"), Some(UrlTarget::ContinueConversation));
        assert_eq!(classify_url("docs/spec.md"), Some(UrlTarget::ContinueConversation));
        assert_eq!(classify_url("foo.rs"), Some(UrlTarget::ContinueConversation));
    }
}
