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
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => MultiSelectOutcome::Cancel,
            KeyCode::Enter => MultiSelectOutcome::Confirm(self.confirm_selection()),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Up => self.move_cursor(-1),
            _ => MultiSelectOutcome::Stay,
        }
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
}
