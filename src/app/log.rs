//! The expanded folder logs. The lines themselves live in the engine.

use std::fmt::Write;

use iced::widget::text_editor::Content;

use crate::domain::sync::SyncDirId;

use super::SyncativeApp;

impl SyncativeApp {
    /// Materialise a log newest-first, so the freshest entry sits on the editor's top line — `text_editor` can't be pinned to the bottom.
    pub(in crate::app) fn build_log_content(&self, sync_dir_id: SyncDirId) -> Content {
        let mut joined = String::new();
        if let Some(lines) = self.engine.logs().lock().unwrap().get(&sync_dir_id) {
            for (i, line) in lines.iter().rev().enumerate() {
                if i > 0 {
                    joined.push('\n');
                }
                let _ = write!(joined, "{}  {}", line.at, line.text);
            }
        }
        Content::with_text(&joined)
    }
}
