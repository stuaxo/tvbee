//! Menu items: actions a plugin offers to interactive front ends (`tveee ui`)
//! that are too rare or too drastic to have a key of their own. Plain data, so
//! plugins describe them whether or not the UI is built.

use crate::error::Result;
use crate::tv::Tv;

#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct MenuItem {
    /// Heading it's listed under, usually the plugin name.
    pub group: &'static str,
    /// Key that picks it in the menu.
    pub key: char,
    pub label: &'static str,
    /// Ask first, with this warning ("The TV will restart..."). None runs at once.
    pub confirm: Option<&'static str>,
    pub run: Run,
}

/// What choosing an item does. Both return feedback for the status line.
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub enum Run {
    Now(fn(&Tv) -> Result<String>),
    /// Ask for a line of text first (`prompt` says what), then run with it.
    WithText { prompt: &'static str, hint: &'static str, run: fn(&Tv, &str) -> Result<String> },
}

impl MenuItem {
    pub const fn new(group: &'static str, key: char, label: &'static str, run: fn(&Tv) -> Result<String>) -> Self {
        MenuItem { group, key, label, confirm: None, run: Run::Now(run) }
    }

    /// An item that asks for text (`prompt`, with an example `hint`) before running.
    pub const fn with_text(
        group: &'static str,
        key: char,
        label: &'static str,
        prompt: &'static str,
        hint: &'static str,
        run: fn(&Tv, &str) -> Result<String>,
    ) -> Self {
        MenuItem { group, key, label, confirm: None, run: Run::WithText { prompt, hint, run } }
    }

    pub const fn confirm(mut self, warning: &'static str) -> Self {
        self.confirm = Some(warning);
        self
    }
}

pub fn none() -> Vec<MenuItem> {
    Vec::new()
}
