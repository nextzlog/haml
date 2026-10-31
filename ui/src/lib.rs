pub mod icon;
pub mod rig;
pub mod so2r;
pub mod text;

pub use rig::{RigAction, RigPanel, RigView};
pub use so2r::{So2rAction, So2rPanel, So2rView};

use egui::{Align, Color32, Layout, RichText, Sense, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkState {
    #[default]
    Off,
    Connecting,
    Connected,
    Failed,
}

pub(crate) fn alert(v: &egui::Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_rgb(240, 100, 90)
    } else {
        Color32::from_rgb(190, 30, 30)
    }
}

pub(crate) fn good(v: &egui::Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_rgb(110, 210, 130)
    } else {
        Color32::from_rgb(20, 140, 60)
    }
}

pub fn toolbar<R>(ui: &mut Ui, status: &[(&str, bool)], tools: impl FnOnce(&mut Ui) -> R) -> R {
    let r = ui.horizontal_wrapped(tools).inner;
    let shown: Vec<_> = status.iter().filter(|(t, _)| !t.is_empty()).collect();
    if !shown.is_empty() {
        ui.with_layout(
            Layout::right_to_left(Align::Min).with_main_wrap(true),
            |ui| {
                for (t, warn) in shown.iter().rev() {
                    let color = if *warn {
                        ui.visuals().warn_fg_color
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.add(egui::Label::new(RichText::new(*t).color(color)).wrap());
                }
            },
        );
    }
    ui.separator();
    r
}

pub fn connect_tool(ui: &mut Ui, link: LinkState, language: &str) -> Option<bool> {
    let t = |k| text::texts().get(language, k);
    if link == LinkState::Connecting {
        ui.spinner().on_hover_text(t("connecting"));
    }
    let (glyph, key, tip, on) = match link {
        LinkState::Connected | LinkState::Connecting => {
            (icon::PLUGS_CONNECTED, "disconnect", "disconnect.tip", false)
        }
        LinkState::Failed => (icon::PLUGS, "reconnect", "reconnect.tip", true),
        LinkState::Off => (icon::PLUGS, "connect", "connect.tip", true),
    };
    icon::tool(ui, glyph, t(key), t(tip), false)
        .clicked()
        .then_some(on)
}

pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    options: &[(T, &str)],
    tip: &str,
) -> bool {
    let mut changed = false;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (v, label) in options {
            let on = *current == *v;
            let r = ui.add(egui::Button::selectable(on, *label));
            let r = icon::labelled(r, label, on);
            let r = if tip.is_empty() {
                r
            } else {
                r.on_hover_text(tip)
            };
            if r.clicked() && !on {
                *current = *v;
                changed = true;
            }
        }
    });
    changed
}

pub fn led(ui: &mut Ui, on: bool, label: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
    let color = if on {
        good(ui.visuals())
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().circle_filled(rect.center(), 5.0, color);
    ui.label(label);
}
