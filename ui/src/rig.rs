use std::time::Instant;

use egui::{RichText, Ui};
use haml::{Command, Link, Mode, Rig, Vfo};

use crate::icon;

pub struct RigPanel {
    pub name: String,
    rig: Box<dyn Rig>,
    entry: String,
    error: Option<String>,
}

pub fn parse_khz(text: &str) -> Option<u64> {
    let khz: f64 = text.trim().parse().ok()?;
    (khz > 0.0).then(|| (khz * 1000.0).round() as u64)
}

pub fn show_khz(hz: u64) -> String {
    format!("{}.{:03}", hz / 1000, hz % 1000)
}

impl RigPanel {
    pub fn new(name: &str, rig: Box<dyn Rig>) -> Self {
        RigPanel {
            name: name.into(),
            rig,
            entry: String::new(),
            error: None,
        }
    }

    pub fn rig(&mut self) -> &mut dyn Rig {
        self.rig.as_mut()
    }

    pub fn tick(&mut self, now: Instant) {
        self.rig.tick(now);
        for o in self.rig.take_outcomes() {
            self.error = (o.status != haml::Status::Done && o.status != haml::Status::Sent)
                .then(|| format!("{:?}: {:?}", o.command, o.status));
        }
    }

    pub fn tune(&mut self, text: &str) -> bool {
        match parse_khz(text) {
            Some(hz) => {
                let vfo = self.rig.state().vfo;
                self.rig.send(Command::SetFreq(vfo, hz), Instant::now());
                self.error = None;
                true
            }
            None => {
                self.error = Some(format!("not a frequency in kHz: {text}"));
                false
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let state = self.rig.state();
        let up = self.rig.link() == Link::Connected;
        ui.horizontal(|ui| {
            let color = if up {
                crate::good(ui.visuals())
            } else {
                crate::alert(ui.visuals())
            };
            icon::install(ui.ctx());
            ui.label(icon::glyph(icon::CIRCLE).color(color));
            ui.strong(&self.name);
        });
        let freq = state.freq().map_or_else(|| "-".into(), show_khz);
        ui.label(RichText::new(freq).monospace().size(28.0));
        ui.horizontal_wrapped(|ui| {
            for mode in Mode::ALL {
                let on = state.mode == Some(mode);
                if ui.selectable_label(on, mode.label()).clicked() {
                    self.rig
                        .send(Command::SetMode(Vfo::A, mode), Instant::now());
                }
            }
        });
        ui.horizontal(|ui| {
            let name = ui.label("Frequency");
            let r = ui
                .add(egui::TextEdit::singleline(&mut self.entry).desired_width(120.0))
                .labelled_by(name.id);
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if icon::button(ui, icon::PLAY, "kHz", false).clicked() || enter {
                let text = std::mem::take(&mut self.entry);
                self.tune(&text);
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(crate::alert(ui.visuals()), e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haml::sim::{Impairments, Simulator};
    use haml::{Cat, Config, Family};

    fn panel() -> (Simulator, RigPanel) {
        let family = Family::Icom { address: 0x94 };
        let sim = Simulator::new(family, Impairments::clean(), 3);
        sim.realtime(true);
        let cat = Cat::new(family.protocol(), sim.transport(), Config::default());
        (sim, RigPanel::new("IC-7300", Box::new(cat)))
    }

    #[test]
    fn kilohertz_in_and_out() {
        assert_eq!(parse_khz("14025.5"), Some(14_025_500));
        assert_eq!(parse_khz("abc"), None);
        assert_eq!(parse_khz("-1"), None);
        assert_eq!(show_khz(7_012_340), "7012.340");
    }

    #[test]
    fn typing_a_frequency_tunes_the_rig() {
        let (sim, mut p) = panel();
        assert!(p.tune("7012"));
        assert!(!p.tune("x"));
        let start = Instant::now();
        while sim.radio().freq() != 7_012_000 {
            assert!(start.elapsed().as_secs() < 5);
            p.tick(Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn the_panel_shows_the_rig() {
        let (_sim, p) = panel();
        let mut p = Some(p);
        let mut h = egui_kittest::Harness::new_ui(move |ui| p.as_mut().unwrap().ui(ui));
        h.run();
        egui_kittest::kittest::Queryable::get_by_label(&h, "IC-7300");
        egui_kittest::kittest::Queryable::get_by_label(&h, "CW");
    }
}
