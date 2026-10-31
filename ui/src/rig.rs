use egui::{RichText, Ui};
use haml::{Link, Mode, Rig, RigCaps, State};

use crate::text::texts;
use crate::{icon, LinkState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RigAction {
    Connect,
    Disconnect,
    SetUp,
    Select(u8),
    Tune(u64),
    Mode(Mode),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RigView {
    pub info: String,
    pub link: LinkState,
    pub problem: Option<String>,
    pub set_up: bool,
    pub number: u8,
    pub rigs: u8,
    pub state: Option<State>,
    pub caps: RigCaps,
}

impl Default for RigView {
    fn default() -> Self {
        RigView {
            info: String::new(),
            link: LinkState::Off,
            problem: None,
            set_up: false,
            number: 1,
            rigs: 1,
            state: None,
            caps: RigCaps::NONE,
        }
    }
}

impl RigView {
    pub fn of(rig: &dyn Rig, info: &str) -> Self {
        let link = match rig.link() {
            Link::Connected => LinkState::Connected,
            Link::Disconnected => LinkState::Failed,
        };
        RigView {
            info: info.into(),
            link,
            set_up: true,
            number: 1,
            rigs: 1,
            state: Some(rig.state()),
            caps: rig.caps(),
            ..RigView::default()
        }
    }
}

pub fn parse_khz(text: &str) -> Option<u64> {
    let khz: f64 = text.trim().parse().ok()?;
    (khz > 0.0 && khz < 1e9).then(|| (khz * 1000.0).round() as u64)
}

pub fn show_khz(hz: u64) -> String {
    format!("{}.{:03}", hz / 1000, hz % 1000)
}

fn khz(hz: Option<u64>) -> String {
    hz.map_or_else(|| "-".into(), show_khz)
}

#[derive(Debug, Clone, Default)]
pub struct RigPanel {
    pub language: String,
    entry: String,
    error: Option<String>,
}

impl RigPanel {
    pub fn new(language: &str) -> Self {
        RigPanel {
            language: language.into(),
            ..RigPanel::default()
        }
    }

    fn t(&self, key: &'static str) -> &'static str {
        texts().get(&self.language, key)
    }

    pub fn tune(&mut self, text: &str) -> Option<RigAction> {
        match parse_khz(text) {
            Some(hz) => {
                self.error = None;
                Some(RigAction::Tune(hz))
            }
            None => {
                self.error = Some(texts().format(&self.language, "rig.bad_freq", &[text.trim()]));
                None
            }
        }
    }

    pub fn tools(&mut self, ui: &mut Ui, view: &RigView) -> Vec<RigAction> {
        icon::install(ui.ctx());
        let mut out = Vec::new();
        if view.set_up || view.link != LinkState::Off {
            match crate::connect_tool(ui, view.link, &self.language) {
                Some(true) => out.push(RigAction::Connect),
                Some(false) => out.push(RigAction::Disconnect),
                None => {}
            }
        }
        if view.rigs > 1 {
            let mut n = view.number;
            let tip = self.t("rig.choice.tip");
            if crate::segmented(ui, &mut n, &[(1, "RIG1"), (2, "RIG2")], tip) {
                out.push(RigAction::Select(n));
            }
        }
        out
    }

    pub fn body(&mut self, ui: &mut Ui, view: &RigView) -> Vec<RigAction> {
        let mut out = Vec::new();
        if !view.set_up && view.link == LinkState::Off {
            let size = [ui.available_width(), ui.spacing().interact_size.y];
            let b = egui::Button::new(self.t("rig.set_up"));
            if ui
                .add_sized(size, b)
                .on_hover_text(self.t("rig.set_up.tip"))
                .clicked()
            {
                out.push(RigAction::SetUp);
            }
            return out;
        }
        let state = view.state.filter(|_| view.link == LinkState::Connected);
        let Some(st) = state else {
            ui.weak(self.t("rig.not_connected"));
            if let Some(p) = &view.problem {
                ui.colored_label(crate::alert(ui.visuals()), p);
            }
            return out;
        };
        ui.label(RichText::new(khz(st.freq())).monospace().size(28.0));
        let mode = st.mode.map_or("-", |m| m.label());
        egui::Grid::new("haml_rig_state")
            .num_columns(2)
            .striped(true)
            .min_col_width(ui.available_width() / 3.0)
            .show(ui, |ui| {
                for (k, v) in [
                    (self.t("rig.vfo_a"), khz(st.freq_a)),
                    (self.t("rig.vfo_b"), khz(st.freq_b)),
                    (self.t("rig.mode"), mode.to_string()),
                ] {
                    ui.label(k);
                    ui.monospace(RichText::new(v).size(18.0));
                    ui.end_row();
                }
            });
        if view.caps.mode {
            ui.horizontal_wrapped(|ui| {
                for m in Mode::ALL {
                    let on = st.mode == Some(m);
                    let r = ui.selectable_label(on, m.label());
                    if icon::labelled(r, m.label(), on).clicked() && !on {
                        out.push(RigAction::Mode(m));
                    }
                }
            });
        }
        if view.caps.freq {
            let hint = self.t("rig.freq");
            let hover = self.t("rig.freq.tip");
            let tune = self.t("rig.tune");
            let tip = self.t("rig.tune.tip");
            ui.horizontal(|ui| {
                let room = ui.available_width() - 90.0;
                let r = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.entry)
                            .hint_text(hint)
                            .desired_width(room.max(60.0)),
                    )
                    .on_hover_text(hover);
                let r = icon::labelled(r, hint, false);
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if icon::tool(ui, icon::PLAY, tune, tip, false).clicked() || enter {
                    let text = std::mem::take(&mut self.entry);
                    out.extend(self.tune(&text));
                }
            });
        }
        if let Some(e) = view.problem.as_ref().or(self.error.as_ref()) {
            ui.colored_label(crate::alert(ui.visuals()), e);
        }
        out
    }

    pub fn ui(&mut self, ui: &mut Ui, view: &RigView) -> Vec<RigAction> {
        let problem = view.problem.clone().unwrap_or_default();
        let mut out = crate::toolbar(ui, &[(&view.info, false), (&problem, true)], |ui| {
            self.tools(ui, view)
        });
        out.extend(self.body(ui, view));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use haml::sim::{Impairments, Simulator};
    use haml::{Cat, Command, Config, Family, Vfo};
    use std::time::Instant;

    fn rig() -> (Simulator, Cat<haml::sim::SimTransport>) {
        let family = Family::Icom { address: 0x94 };
        let sim = Simulator::new(family, Impairments::clean(), 3);
        sim.realtime(true);
        let cat = Cat::new(family.protocol(), sim.transport(), Config::default());
        (sim, cat)
    }

    fn connected() -> RigView {
        let st = State {
            freq_a: Some(7_012_340),
            mode: Some(Mode::Cw),
            ..State::default()
        };
        RigView {
            info: "IC-7300".into(),
            link: LinkState::Connected,
            set_up: true,
            number: 1,
            rigs: 2,
            state: Some(st),
            caps: RigCaps::ALL,
            ..RigView::default()
        }
    }

    fn harness(
        view: RigView,
        lang: &str,
        width: f32,
    ) -> Harness<'static, (RigPanel, Vec<RigAction>)> {
        let mut h = Harness::builder()
            .with_size(egui::vec2(width, 800.0))
            .build_ui_state(
                move |ui: &mut egui::Ui, s: &mut (RigPanel, Vec<RigAction>)| {
                    ui.ctx()
                        .set_missing_glyph_policy(egui::epaint::text::MissingGlyphPolicy::Tofu);
                    let a = s.0.ui(ui, &view);
                    s.1.extend(a);
                },
                (RigPanel::new(lang), Vec::new()),
            );
        h.run();
        h
    }

    #[test]
    fn kilohertz_in_and_out() {
        assert_eq!(parse_khz("14025.5"), Some(14_025_500));
        assert_eq!(parse_khz("abc"), None);
        assert_eq!(parse_khz("-1"), None);
        assert_eq!(show_khz(7_012_340), "7012.340");
        let mut p = RigPanel::new("ja");
        assert_eq!(p.tune("7012"), Some(RigAction::Tune(7_012_000)));
        assert_eq!(p.tune("x"), None);
        assert!(p.error.as_deref().unwrap().contains("kHz の周波数"));
    }

    #[test]
    fn a_connected_rig_shows_its_dial_and_offers_disconnect() {
        let mut h = harness(connected(), "en", 280.0);
        assert_eq!(h.query_all_by_label("7012.340").count(), 2);
        h.get_by_label("Disconnect").click();
        h.get_by_label("RIG2").click();
        h.get_by_label("USB").click();
        h.run();
        assert_eq!(
            h.state().1,
            [
                RigAction::Disconnect,
                RigAction::Select(2),
                RigAction::Mode(Mode::Usb)
            ]
        );
        assert!(h.query_by_label("Connect").is_none());
    }

    #[test]
    fn a_rig_not_set_up_offers_set_up_only() {
        let view = RigView::default();
        let mut h = harness(view, "ja", 280.0);
        assert!(h.query_by_label("接続").is_none());
        h.get_by_label("リグの設定").click();
        h.run();
        assert_eq!(h.state().1, [RigAction::SetUp]);
    }

    #[test]
    fn a_failed_rig_explains_and_offers_reconnect() {
        let view = RigView {
            link: LinkState::Failed,
            set_up: true,
            problem: Some("COM3 not found".into()),
            ..RigView::default()
        };
        let mut h = harness(view, "en", 280.0);
        assert!(h.query_all_by_label("COM3 not found").count() >= 1);
        h.get_by_label("Disconnected");
        h.get_by_label("Reconnect").click();
        h.run();
        assert_eq!(h.state().1, [RigAction::Connect]);
    }

    #[test]
    fn every_control_fits_a_280_wide_dock() {
        for lang in ["en", "ja"] {
            let h = harness(connected(), lang, 280.0);
            for n in h.query_all_by(|n| {
                matches!(
                    n.role(),
                    egui::accesskit::Role::Button | egui::accesskit::Role::TextInput
                )
            }) {
                let r = n.rect();
                assert!(r.max.x <= 280.5 && r.min.x >= -0.5, "{r:?} {lang}");
            }
        }
    }

    #[test]
    fn a_typed_frequency_tunes_a_real_rig() {
        let (sim, mut cat) = rig();
        let mut p = RigPanel::new("en");
        if let Some(RigAction::Tune(hz)) = p.tune("7012") {
            cat.send(Command::SetFreq(Vfo::A, hz), Instant::now());
        }
        let start = Instant::now();
        while sim.radio().freq() != 7_012_000 {
            assert!(start.elapsed().as_secs() < 5);
            cat.tick(Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let v = RigView::of(&cat, "IC-7300");
        assert!(v.set_up && v.caps.freq);
    }
}
