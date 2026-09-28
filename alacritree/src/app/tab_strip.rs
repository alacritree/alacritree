//! Session-strip painting independent of session ownership. A click reports
//! a request; only the app decides whether to switch or start a shell.

use super::*;

pub(super) struct Tab<'a> {
    pub id: SessionId,
    pub title: &'a str,
    pub active: bool,
    pub attention: bool,
}

#[derive(Default)]
pub(super) struct Requests {
    pub activate: Option<SessionId>,
    pub spawn_default: bool,
    pub spawn_profile: Option<String>,
}

pub(super) fn show(
    ui: &mut egui::Ui,
    tabs: &[Tab<'_>],
    profile_names: &[String],
    theme: &Theme,
) -> Requests {
    let mut requests = Requests::default();
    // Reserve a 2px-tall strip across the full width of the terminal pane.
    let strip_height = 2.0;
    let gap = 4.0;
    let plus_width = 12.0;
    let avail = ui.available_width();
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(avail, strip_height + 2.0), egui::Sense::hover());
    let seg_avail = avail - plus_width - gap;
    let segment_width =
        ((seg_avail - gap * (tabs.len() as f32 - 1.0)) / tabs.len().max(1) as f32).max(1.0);
    for (i, tab) in tabs.iter().enumerate() {
        let x0 = rect.min.x + i as f32 * (segment_width + gap);
        let seg_rect = egui::Rect::from_min_size(
            egui::pos2(x0, rect.min.y + 1.0),
            egui::vec2(segment_width, strip_height),
        );
        // 2px is too small to reliably click, so expand the hit zone vertically.
        let click_rect = seg_rect.expand2(egui::vec2(0.0, 4.0));
        let resp =
            ui.interact(click_rect, ui.id().with(("tab_strip", tab.id)), egui::Sense::click());
        // Attention wins over the active/inactive shading so a bell from a
        // non-active tab pulls the eye even when another tab is selected.
        let color = if tab.attention {
            theme.attention
        } else if tab.active {
            theme.text
        } else if resp.hovered() {
            theme.text_dim
        } else {
            theme.text_muted
        };
        ui.painter().rect_filled(seg_rect, 0.0, color);
        if resp.clicked() {
            requests.activate = Some(tab.id);
        }
        resp.on_hover_text(tab.title);
    }
    let plus_rect = egui::Rect::from_min_size(
        egui::pos2(rect.max.x - plus_width, rect.min.y + 1.0),
        egui::vec2(plus_width, strip_height),
    );
    let click_rect = plus_rect.expand2(egui::vec2(0.0, 4.0));
    let resp = ui.interact(click_rect, ui.id().with("tab_strip_plus"), egui::Sense::click());
    let color = if resp.hovered() { theme.text_dim } else { theme.text_muted };
    ui.painter().rect_filled(plus_rect, 0.0, color);
    requests.spawn_default = resp.clicked();
    if !profile_names.is_empty() {
        resp.context_menu(|ui| {
            ui.label(RichText::new("New session with…").color(theme.text_muted).small());
            for name in profile_names {
                if ui.button(name).clicked() {
                    requests.spawn_profile = Some(name.clone());
                    ui.close_menu();
                }
            }
        });
    }
    resp.on_hover_text(if profile_names.is_empty() {
        "New session"
    } else {
        "New session (right-click: profiles)"
    });
    requests
}
