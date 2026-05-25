//! Renderer-side default-action interceptor. Runs AFTER raw event
//! dispatch (`events::dispatch_raw`). Inspects the targeted DOM node;
//! if it matches a default-action element (link, summary, form button),
//! produces an `Effect` for the host to apply.
//!
//! Why renderer-side: keeps Blitz's headless eventing self-contained.
//! Effects flow up via `InputOutcome::effects` so the host still
//! mediates the actual shell-out.

#![warn(missing_docs)]

use blitz_dom::{BaseDocument, ElementData, local_name};
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
