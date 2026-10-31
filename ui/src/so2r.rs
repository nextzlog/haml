use egui::Ui;
use haml::Rx;

use crate::text::texts;
use crate::{icon, LinkState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum So2rAction {
    Connect,
    Disconnect,
    SetUp,
    Rx(Rx),
    AutoRx,
    Blend,
    ToggleTx,
    CancelRx,
    Ratio(u8),
    Send(String),
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct So2rView {
    pub link: LinkState,
    pub set_up: bool,
    pub status: String,
    pub problem: String,
    pub rx: Rx,
    pub auto_rx: bool,
    pub blend: bool,
    pub cancel_rx: bool,
    pub ptt: bool,
    pub tx: u8,
    pub ratio: u8,
    pub message: bool,
}

impl Default for So2rView {
    fn default() -> Self {
        So2rView {
            link: LinkState::Off,
            set_up: false,
            status: String::new(),
            problem: String::new(),
            rx: Rx::Rig1,
            auto_rx: false,
            blend: false,
            cancel_rx: false,
            ptt: false,
            tx: 1,
            ratio: 50,
            message: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct So2rPanel {
    pub language: String,
    message: String,
}

impl So2rPanel {
    pub fn new(language: &str) -> Self {
        So2rPanel {
            language: language.into(),
            message: String::new(),
        }
    }

    fn t(&self, key: &'static str) -> &'static str {
        texts().get(&self.language, key)
    }

    fn toggle(&self, ui: &mut Ui, glyph: &str, keys: [&'static str; 2], on: bool) -> bool {
        icon::tool(ui, glyph, self.t(keys[0]), self.t(keys[1]), on).clicked()
    }

    pub fn tools(&mut self, ui: &mut Ui, view: &So2rView) -> Vec<So2rAction> {
        icon::install(ui.ctx());
        let mut out = Vec::new();
        if view.set_up || view.link != LinkState::Off {
            match crate::connect_tool(ui, view.link, &self.language) {
                Some(true) => out.push(So2rAction::Connect),
                Some(false) => out.push(So2rAction::Disconnect),
                None => {}
            }
        }
        let mut rx = view.rx;
        let options = [
            (Rx::Rig1, "RIG1"),
            (Rx::Rig2, "RIG2"),
            (Rx::Both, self.t("so2r.both")),
        ];
        if crate::segmented(ui, &mut rx, &options, self.t("so2r.rig.tip")) {
            out.push(So2rAction::Rx(rx));
        }
        if self.toggle(
            ui,
            icon::ARROWS_LEFT_RIGHT,
            ["so2r.auto_rx", "so2r.auto_rx.tip"],
            view.auto_rx,
        ) {
            out.push(So2rAction::AutoRx);
        }
        ui.add_enabled_ui(view.rx == Rx::Both, |ui| {
            if self.toggle(
                ui,
                icon::WAVEFORM,
                ["so2r.blend", "so2r.blend.tip"],
                view.blend,
            ) {
                out.push(So2rAction::Blend);
            }
        });
        if self.toggle(
            ui,
            icon::SWAP,
            ["so2r.toggle_tx", "so2r.toggle_tx.tip"],
            false,
        ) {
            out.push(So2rAction::ToggleTx);
        }
        if self.toggle(
            ui,
            icon::X_CIRCLE,
            ["so2r.cancel_rx", "so2r.cancel_rx.tip"],
            view.cancel_rx,
        ) {
            out.push(So2rAction::CancelRx);
        }
        out
    }

    pub fn body(&mut self, ui: &mut Ui, view: &So2rView) -> Vec<So2rAction> {
        let mut out = Vec::new();
        if !view.set_up && view.link == LinkState::Off {
            ui.weak(self.t("so2r.none"));
            let size = [ui.available_width(), ui.spacing().interact_size.y];
            let b = egui::Button::new(self.t("so2r.set_up"));
            if ui
                .add_sized(size, b)
                .on_hover_text(self.t("so2r.set_up.tip"))
                .clicked()
            {
                out.push(So2rAction::SetUp);
            }
            return out;
        }
        ui.horizontal_wrapped(|ui| {
            crate::led(ui, view.ptt, "PTT");
            for rig in [1, 2] {
                crate::led(ui, view.tx == rig, &format!("TX{rig}"));
            }
        });
        ui.add_enabled_ui(view.rx == Rx::Both, |ui| {
            let tip = self.t("so2r.ratio.tip");
            ui.columns(3, |cols| {
                for (col, p) in cols.iter_mut().zip([100u8, 50, 0]) {
                    let b = egui::Button::new(format!("{p}%"));
                    let size = [col.available_width(), col.spacing().interact_size.y];
                    let r = col.add_sized(size, b);
                    if r.on_hover_text(tip).clicked() {
                        out.push(So2rAction::Ratio(p));
                    }
                }
            });
            let mut ratio = view.ratio;
            ui.spacing_mut().slider_width = (ui.available_width() - 64.0).max(60.0);
            let r = ui
                .add(egui::Slider::new(&mut ratio, 0..=100).suffix("%"))
                .on_hover_text(tip);
            if r.changed() && ratio != view.ratio {
                out.push(So2rAction::Ratio(ratio));
            }
        });
        if view.message {
            let hint = self.t("so2r.message");
            ui.horizontal(|ui| {
                let room = ui.available_width() - 90.0;
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.message)
                        .hint_text(hint)
                        .desired_width(room.max(60.0)),
                );
                icon::labelled(r, hint, false);
                let tip = texts().get(&self.language, "so2r.send.tip");
                let label = texts().get(&self.language, "so2r.send");
                let send = icon::tool(ui, icon::PLAY, label, tip, false);
                if send.clicked() && !self.message.trim().is_empty() {
                    out.push(So2rAction::Send(self.message.trim().into()));
                }
            });
            let tip = self.t("so2r.stop.tip");
            if icon::tool(ui, icon::STOP, self.t("so2r.stop"), tip, false).clicked() {
                out.push(So2rAction::Stop);
            }
        }
        out
    }

    pub fn ui(&mut self, ui: &mut Ui, view: &So2rView) -> Vec<So2rAction> {
        let status = [(view.status.as_str(), false), (view.problem.as_str(), true)];
        let mut out = crate::toolbar(ui, &status, |ui| self.tools(ui, view));
        out.extend(self.body(ui, view));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    fn harness(
        view: So2rView,
        lang: &str,
        width: f32,
    ) -> Harness<'static, (So2rPanel, Vec<So2rAction>)> {
        let mut h = Harness::builder()
            .with_size(egui::vec2(width, 800.0))
            .build_ui_state(
                move |ui: &mut egui::Ui, s: &mut (So2rPanel, Vec<So2rAction>)| {
                    ui.ctx()
                        .set_missing_glyph_policy(egui::epaint::text::MissingGlyphPolicy::Tofu);
                    let a = s.0.ui(ui, &view);
                    s.1.extend(a);
                },
                (So2rPanel::new(lang), Vec::new()),
            );
        h.run();
        h
    }

    fn connected() -> So2rView {
        So2rView {
            link: LinkState::Connected,
            set_up: true,
            rx: Rx::Both,
            ..So2rView::default()
        }
    }

    #[test]
    fn toolbar_clicks_become_actions() {
        let mut h = harness(connected(), "en", 280.0);
        for l in [
            "RIG2",
            "Auto RX",
            "AF blend",
            "Toggle TX",
            "Cancel RX",
            "100%",
            "Disconnect",
        ] {
            h.get_by_label(l).click();
            h.run();
        }
        assert_eq!(
            h.state().1,
            [
                So2rAction::Rx(Rx::Rig2),
                So2rAction::AutoRx,
                So2rAction::Blend,
                So2rAction::ToggleTx,
                So2rAction::CancelRx,
                So2rAction::Ratio(100),
                So2rAction::Disconnect,
            ]
        );
    }

    #[test]
    fn without_a_box_the_panel_offers_set_up() {
        let mut h = harness(So2rView::default(), "ja", 280.0);
        h.get_by_label("SO2R 装置なし");
        assert!(h.query_by_label("接続").is_none());
        h.get_by_label("SO2R の設定").click();
        h.run();
        assert_eq!(h.state().1, [So2rAction::SetUp]);
    }

    #[test]
    fn blend_is_only_for_both() {
        let view = So2rView {
            rx: Rx::Rig1,
            ..connected()
        };
        let mut h = harness(view, "en", 600.0);
        h.get_by_label("AF blend").click();
        h.run();
        assert!(h.state().1.is_empty());
    }

    #[test]
    fn the_standalone_message_row_sends_and_stops() {
        let view = So2rView {
            message: true,
            ..connected()
        };
        let mut h = harness(view, "en", 280.0);
        h.get_by_label("Message").click();
        h.run();
        h.get_by_label("Message").type_text("TEST");
        h.run();
        h.get_by_label("Send CW").click();
        h.run();
        h.get_by_label("Stop").click();
        h.run();
        assert_eq!(
            h.state().1,
            [So2rAction::Send("TEST".into()), So2rAction::Stop]
        );
    }

    #[test]
    fn every_control_fits_a_280_wide_dock() {
        for lang in ["en", "ja"] {
            let h = harness(connected(), lang, 280.0);
            for n in h.query_all_by(|n| n.role() == egui::accesskit::Role::Button) {
                let r = n.rect();
                assert!(r.max.x <= 280.5 && r.min.x >= -0.5, "{r:?} {lang}");
            }
        }
    }
}
