use std::time::Instant;

use egui::Ui;
use haml::so2r::{Operator, RxMode};
use haml::tx::Signal;
use haml::{TxCommand, TxEvent};

use crate::icon;

#[derive(Default)]
pub struct So2rPanel {
    pub operator: Operator,
    commands: Vec<TxCommand>,
    message: String,
    error: Option<String>,
}

impl So2rPanel {
    pub fn take_commands(&mut self, now: Instant) -> Vec<TxCommand> {
        let mut c = std::mem::take(&mut self.commands);
        c.extend(self.operator.tick(now));
        c
    }

    pub fn on_event(&mut self, event: &TxEvent, now: Instant) {
        if let TxEvent::Error(e) = event {
            self.error = Some(e.clone());
        }
        let follow = self.operator.on_event(event, now);
        self.commands.extend(follow);
    }

    pub fn stop(&mut self) {
        self.operator.stop_alternate();
        self.commands.push(TxCommand::Stop);
    }

    pub fn toolbar_ui(&mut self, ui: &mut Ui) {
        let label = format!("RIG{}", self.operator.focus());
        if icon::button(ui, icon::ARROWS_LEFT_RIGHT, &label, false).clicked() {
            let c = self.operator.toggle_focus();
            self.commands.extend(c);
        }
        if icon::button(ui, icon::STOP, "Stop", false).clicked() {
            self.stop();
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("RX");
            for (mode, label) in [
                (RxMode::Focus, "Focus"),
                (RxMode::Both, "Both"),
                (RxMode::Other, "Other"),
            ] {
                if ui.selectable_label(false, label).clicked() {
                    let c = self.operator.set_rx(mode);
                    self.commands.extend(c);
                }
            }
        });
        ui.horizontal(|ui| {
            let name = ui.label("Message");
            ui.add(egui::TextEdit::singleline(&mut self.message).desired_width(200.0))
                .labelled_by(name.id);
            if icon::button(ui, icon::PLAY, "CW", false).clicked() && !self.message.is_empty() {
                let c = self.operator.send(Signal::Cw(self.message.clone()));
                self.commands.push(c);
            }
        });
        let air = self
            .operator
            .on_air()
            .map_or_else(|| "-".to_string(), |r| format!("RIG{r}"));
        ui.label(format!("TX {air}"));
        if let Some(e) = &self.error {
            ui.colored_label(crate::alert(ui.visuals()), e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haml::Rx;

    #[test]
    fn the_panel_turns_clicks_into_tx_commands() {
        let mut p = So2rPanel::default();
        let now = Instant::now();
        let c = p.operator.toggle_focus();
        p.commands.extend(c);
        p.stop();
        assert_eq!(
            p.take_commands(now),
            [
                TxCommand::Focus(2),
                TxCommand::Listen(Rx::Rig2),
                TxCommand::Stop
            ]
        );
        p.on_event(&TxEvent::Error("no radio 3".into()), now);
        assert!(p.error.is_some());
    }

    #[test]
    fn the_toolbar_has_focus_and_stop() {
        let mut p = Some(So2rPanel::default());
        let mut h = egui_kittest::Harness::new_ui(move |ui| {
            ui.horizontal(|ui| p.as_mut().unwrap().toolbar_ui(ui));
        });
        h.run();
        egui_kittest::kittest::Queryable::get_by_label(&h, "Stop");
        egui_kittest::kittest::Queryable::get_by_label(&h, "RIG1");
    }
}
