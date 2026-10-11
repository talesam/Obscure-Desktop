//! Helpers for the terminal-like text views (raw log, live connections):
//! bounded buffer and "follow the tail" only when the user is at the end.

use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;

/// Lines kept in a dialog's text buffer.
pub const MAX_VIEW_LINES: i32 = 1000;

/// `true` when the view shows the end (so new lines should follow).
pub fn is_at_bottom(scrolled: &gtk::ScrolledWindow) -> bool {
    let adj = scrolled.vadjustment();
    adj.value() + adj.page_size() >= adj.upper() - 4.0
}

/// Appends `insert`'s text as a new line, drops the oldest lines above
/// `MAX_VIEW_LINES` and keeps following the tail if it was being followed.
pub fn append_line(
    view: &gtk::TextView,
    scrolled: &gtk::ScrolledWindow,
    insert: impl FnOnce(&gtk::TextBuffer, &mut gtk::TextIter),
) {
    let follow = is_at_bottom(scrolled);
    let buffer = view.buffer();
    let mut end = buffer.end_iter();
    if buffer.char_count() > 0 {
        buffer.insert(&mut end, "\n");
    }
    insert(&buffer, &mut end);
    trim(&buffer, MAX_VIEW_LINES);
    if follow {
        scroll_to_end(scrolled);
    }
}

/// Deletes the oldest lines so at most `max_lines` remain.
pub fn trim(buffer: &gtk::TextBuffer, max_lines: i32) {
    let extra = buffer.line_count() - max_lines;
    if extra > 0
        && let Some(mut cut) = buffer.iter_at_line(extra)
    {
        let mut start = buffer.start_iter();
        buffer.delete(&mut start, &mut cut);
    }
}

pub fn scroll_to_end(scrolled: &gtk::ScrolledWindow) {
    glib::idle_add_local_once(clone!(
        #[weak]
        scrolled,
        move || {
            let adj = scrolled.vadjustment();
            adj.set_value(adj.upper() - adj.page_size());
        }
    ));
}
