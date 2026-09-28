//! The last line of the left sidebar, which paints a [`StatusLine`]. What it
//! says is decided in `crate::activity`; this only lays it out.
//!
//! The row keeps its height whether or not it has anything to say, so the
//! project list above it never moves.

use egui::{Sense, TopBottomPanel};

use crate::activity::StatusLine;

use super::*;

/// Draw `line` as a bottom panel inside the left sidebar. Nested bottom
/// panels stack upward from the first one drawn, so this goes before the
/// docked tasks to sit below them.
pub(super) fn show(ui: &mut egui::Ui, line: &StatusLine, theme: &Theme) {
    let height = ui.text_style_height(&egui::TextStyle::Small) + 6.0 * theme.ui_scale;
    // Panel IDs are global; include the parent so catalog previews stay distinct.
    TopBottomPanel::bottom(ui.id().with("activity_row"))
        .resizable(false)
        .exact_height(height)
        .frame(Frame::default())
        .show_inside(ui, |ui| {
            let row = ui.horizontal_centered(|ui| {
                if line.running {
                    braille_loader(ui, 10.0 * theme.ui_scale, theme.accent);
                }
                let color = if line.failed { theme.error } else { theme.text_dim };
                ui.add(egui::Label::new(RichText::new(&line.text).small().color(color)).truncate());
            });
            if !line.tooltip.is_empty() {
                let hover = ui.interact(row.response.rect, ui.id().with("hover"), Sense::hover());
                hover.on_hover_text(line.tooltip.join("\n"));
            }
        });
    // Never at frame rate: once a second while something runs, once a minute
    // while an age shows.
    if let Some(after) = line.repaint_after {
        ui.ctx().request_repaint_after(after);
    }
}
