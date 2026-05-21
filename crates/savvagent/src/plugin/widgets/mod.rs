//! Reusable UI state-machine helpers that screens can wrap.
//!
//! Plugins live under `plugin::builtin::*`; widgets here are pure
//! state machines with no `Plugin`/`Screen` trait impl. A screen wraps
//! a widget by holding it in a field and translating the widget's
//! outcome enum into closed-vocabulary `Effect`s.

pub mod multi_select_list;
