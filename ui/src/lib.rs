pub mod icon;
pub mod rig;
pub mod so2r;

pub use rig::RigPanel;
pub use so2r::So2rPanel;

use egui::{Color32, Visuals};

pub(crate) fn alert(v: &Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_rgb(240, 100, 90)
    } else {
        Color32::from_rgb(190, 30, 30)
    }
}

pub(crate) fn good(v: &Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_rgb(110, 210, 130)
    } else {
        Color32::from_rgb(20, 140, 60)
    }
}
