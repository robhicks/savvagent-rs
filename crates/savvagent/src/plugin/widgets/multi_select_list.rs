//! Generic multi-select list state.

use std::collections::BTreeSet;

pub struct MultiSelectList<T> {
    items: Vec<T>,
    filter: String,
    cursor: usize,
    selected_ids: BTreeSet<String>,
    filter_fn: Box<dyn Fn(&T, &str) -> bool + Send>,
    id_fn: Box<dyn Fn(&T) -> String + Send>,
}

impl<T> std::fmt::Debug for MultiSelectList<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiSelectList")
            .field("items_len", &self.items.len())
            .field("filter", &self.filter)
            .field("cursor", &self.cursor)
            .field("selected_ids", &self.selected_ids)
            .finish()
    }
}

impl<T> MultiSelectList<T> {
    pub fn new(
        items: Vec<T>,
        filter_fn: impl Fn(&T, &str) -> bool + Send + 'static,
        id_fn: impl Fn(&T) -> String + Send + 'static,
    ) -> Self {
        Self {
            items,
            filter: String::new(),
            cursor: 0,
            selected_ids: BTreeSet::new(),
            filter_fn: Box::new(filter_fn),
            id_fn: Box::new(id_fn),
        }
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selected(&self) -> &BTreeSet<String> {
        &self.selected_ids
    }

    /// Items matching the current filter, in catalog order. Empty filter
    /// returns every item. Allocates a fresh `Vec` of references each call
    /// — fine for the picker's <100-item catalog; revisit if a future
    /// consumer pushes more rows.
    pub fn filtered(&self) -> Vec<&T> {
        let f = self.filter.as_str();
        if f.is_empty() {
            self.items.iter().collect()
        } else {
            self.items.iter().filter(|i| (self.filter_fn)(i, f)).collect()
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MultiSelectOutcome<T: Clone> {
    Stay,
    Preview(T),
    Toggle(T),
    Confirm(Vec<T>),
    Cancel,
}

impl<T: Clone> MultiSelectList<T> {
    /// Confirm-by-walking-items: returns selected items in catalog order,
    /// regardless of selection sequence.
    fn confirm_selection(&self) -> Vec<T> {
        self.items
            .iter()
            .filter(|i| self.selected_ids.contains(&(self.id_fn)(i)))
            .cloned()
            .collect()
    }

    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> MultiSelectOutcome<T> {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Esc => MultiSelectOutcome::Cancel,
            KeyCode::Enter => MultiSelectOutcome::Confirm(self.confirm_selection()),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char(' ') => self.toggle_cursor(),
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    return MultiSelectOutcome::Stay;
                }
                self.clamp_after_filter_change()
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.filter.push(c);
                self.clamp_after_filter_change()
            }
            _ => MultiSelectOutcome::Stay,
        }
    }

    fn clamp_after_filter_change(&mut self) -> MultiSelectOutcome<T> {
        let (cursor, preview) = {
            let filtered = self.filtered();
            if filtered.is_empty() {
                return MultiSelectOutcome::Stay;
            }
            let cursor = if self.cursor >= filtered.len() {
                filtered.len() - 1
            } else {
                self.cursor
            };
            (cursor, filtered[cursor].clone())
        };
        self.cursor = cursor;
        MultiSelectOutcome::Preview(preview)
    }

    fn toggle_cursor(&mut self) -> MultiSelectOutcome<T> {
        let (id, item_clone) = {
            let filtered = self.filtered();
            let Some(item) = filtered.get(self.cursor) else {
                return MultiSelectOutcome::Stay;
            };
            ((self.id_fn)(item), (*item).clone())
        };
        if !self.selected_ids.remove(&id) {
            self.selected_ids.insert(id);
        }
        MultiSelectOutcome::Toggle(item_clone)
    }

    fn move_cursor(&mut self, delta: isize) -> MultiSelectOutcome<T> {
        let (new_cursor, preview) = {
            let filtered = self.filtered();
            if filtered.is_empty() {
                return MultiSelectOutcome::Stay;
            }
            let last = filtered.len() - 1;
            let new = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
            (new, filtered[new].clone())
        };
        self.cursor = new_cursor;
        MultiSelectOutcome::Preview(preview)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Item {
        id: &'static str,
        label: &'static str,
    }

    fn items() -> Vec<Item> {
        vec![
            Item { id: "a", label: "alpha" },
            Item { id: "b", label: "bravo" },
            Item { id: "c", label: "charlie" },
        ]
    }

    fn list() -> MultiSelectList<Item> {
        MultiSelectList::new(
            items(),
            |i: &Item, f: &str| i.label.contains(f),
            |i: &Item| i.id.to_string(),
        )
    }

    #[test]
    fn new_starts_empty_filter_zero_cursor_no_selection() {
        let l = list();
        assert_eq!(l.filter(), "");
        assert_eq!(l.cursor(), 0);
        assert!(l.selected().is_empty());
        assert_eq!(l.filtered().len(), 3);
    }

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn esc_emits_cancel() {
        let mut l = list();
        assert!(matches!(l.on_key(key(KeyCode::Esc)), MultiSelectOutcome::Cancel));
    }

    #[test]
    fn unknown_key_emits_stay() {
        let mut l = list();
        assert!(matches!(
            l.on_key(key(KeyCode::F(5))),
            MultiSelectOutcome::Stay
        ));
    }

    #[test]
    fn enter_with_no_selection_returns_empty_confirm() {
        let mut l = list();
        match l.on_key(key(KeyCode::Enter)) {
            MultiSelectOutcome::Confirm(v) => assert!(v.is_empty()),
            other => panic!("expected Confirm([]), got {other:?}"),
        }
    }

    #[test]
    fn down_moves_cursor_and_emits_preview() {
        let mut l = list();
        let outcome = l.on_key(key(KeyCode::Down));
        assert_eq!(l.cursor(), 1);
        assert!(matches!(outcome, MultiSelectOutcome::Preview(Item { id: "b", .. })));
    }

    #[test]
    fn down_clamps_at_last_row() {
        let mut l = list();
        l.on_key(key(KeyCode::Down));
        l.on_key(key(KeyCode::Down));
        let outcome = l.on_key(key(KeyCode::Down));
        assert_eq!(l.cursor(), 2);
        assert!(matches!(outcome, MultiSelectOutcome::Preview(Item { id: "c", .. })));
    }

    #[test]
    fn up_clamps_at_first_row() {
        let mut l = list();
        let outcome = l.on_key(key(KeyCode::Up));
        assert_eq!(l.cursor(), 0);
        assert!(matches!(outcome, MultiSelectOutcome::Preview(Item { id: "a", .. })));
    }

    #[test]
    fn space_toggles_cursor_item() {
        let mut l = list();
        let out = l.on_key(key(KeyCode::Char(' ')));
        assert!(matches!(out, MultiSelectOutcome::Toggle(Item { id: "a", .. })));
        assert!(l.selected().contains("a"));

        let out = l.on_key(key(KeyCode::Char(' ')));
        assert!(matches!(out, MultiSelectOutcome::Toggle(Item { id: "a", .. })));
        assert!(!l.selected().contains("a"));
    }

    #[test]
    fn typing_chars_narrows_filter() {
        let mut l = list();
        l.on_key(key(KeyCode::Char('l')));
        assert_eq!(l.filter(), "l");
        let filtered: Vec<&str> = l.filtered().iter().map(|i| i.id).collect();
        assert_eq!(filtered, vec!["a", "c"], "alpha + charlie both contain 'l'");
    }

    #[test]
    fn backspace_pops_filter_and_reclamps_cursor() {
        let mut l = list();
        l.on_key(key(KeyCode::Char('l'))); // narrows to [a, c]; cursor 0
        l.on_key(key(KeyCode::Down));      // cursor → 1
        l.on_key(key(KeyCode::Char('p'))); // narrows to [a]; cursor must clamp to 0
        assert_eq!(l.cursor(), 0, "cursor must clamp when filter shrinks past it");
        l.on_key(key(KeyCode::Backspace)); // back to [a, c]
        assert_eq!(l.filter(), "l");
    }

    #[test]
    fn selection_survives_filter_changes() {
        let mut l = list();
        l.on_key(key(KeyCode::Char(' ')));  // select a
        l.on_key(key(KeyCode::Char('b')));  // filter narrows so 'a' is hidden
        assert!(l.filtered().iter().all(|i| i.id != "a"));
        assert!(l.selected().contains("a"), "selection persists across filter");
        l.on_key(key(KeyCode::Backspace));  // restore visibility
        let out = l.on_key(key(KeyCode::Enter));
        match out {
            MultiSelectOutcome::Confirm(items) => {
                assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), vec!["a"]);
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn confirm_returns_items_in_catalog_order_not_selection_order() {
        let mut l = list();
        // Select c first (cursor=2), then a (cursor=0).
        l.on_key(key(KeyCode::Down));
        l.on_key(key(KeyCode::Down));
        l.on_key(key(KeyCode::Char(' '))); // select c
        l.on_key(key(KeyCode::Up));
        l.on_key(key(KeyCode::Up));
        l.on_key(key(KeyCode::Char(' '))); // select a

        let out = l.on_key(key(KeyCode::Enter));
        match out {
            MultiSelectOutcome::Confirm(items) => {
                let ids: Vec<&str> = items.iter().map(|i| i.id).collect();
                assert_eq!(ids, vec!["a", "c"], "must be catalog order, not selection order");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }
}
