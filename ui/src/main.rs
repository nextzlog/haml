#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::any::Any;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haml::rig::{RigClient, RigDriver};
use haml::sim::{Impairments, SimKeyer, SimSo2r, Simulator};
use haml::so2r::{Operator, RxMode};
use haml::tx::Delays;
use haml::tx::Signal;
use haml::{Arbiter, Cat, Command, Config, Device, Family, Rig, RigCaps, RigEvent, Timing, Vfo};
use haml::{Rx, TxCommand, TxEvent};
use haml_ui::{icon, LinkState, RigAction, RigPanel, RigView, So2rAction, So2rPanel, So2rView};

type Sender = Box<dyn Fn(TxCommand)>;

struct App {
    rigs: Vec<(String, Box<dyn Rig>)>,
    panel: RigPanel,
    so2r: So2rPanel,
    operator: Operator,
    ptt: bool,
    error: String,
    send: Sender,
    events: Receiver<TxEvent>,
    _keep: Vec<Box<dyn Any>>,
}

type Rigs = Vec<(String, Device<Command, RigEvent>)>;

fn simulated(keep: &mut Vec<Box<dyn Any>>) -> Rigs {
    [("RIG1", 0x94), ("RIG2", 0x98)]
        .into_iter()
        .map(|(name, address)| {
            let family = Family::Icom { address };
            let sim = Simulator::new(family, Impairments::clean(), u64::from(address));
            sim.realtime(true);
            let cat = Cat::new(family.protocol(), sim.transport(), Config::default());
            keep.push(Box::new(sim));
            (name.to_string(), RigDriver::spawn(cat))
        })
        .collect()
}

fn local() -> App {
    let mut keep: Vec<Box<dyn Any>> = Vec::new();
    let devices = simulated(&mut keep);
    let rigs = devices
        .iter()
        .map(|(name, d)| {
            let rig: Box<dyn Rig> = Box::new(RigClient::new(d, RigCaps::ALL));
            (name.clone(), rig)
        })
        .collect();
    let tx = Arbiter::new(2, Delays::default())
        .with_keyer(Box::new(SimKeyer::new(Timing::default())))
        .with_so2r(Box::new(SimSo2r::new(Arc::new(Mutex::new(Vec::new())))))
        .spawn();
    let events = tx.subscribe();
    let commands = tx.commander();
    keep.push(Box::new(devices));
    keep.push(Box::new(tx));
    App {
        rigs,
        panel: RigPanel::new(&language()),
        so2r: So2rPanel::new(&language()),
        operator: Operator::default(),
        ptt: false,
        error: String::new(),
        send: Box::new(move |command| {
            let _ = commands.send(command);
        }),
        events,
        _keep: keep,
    }
}

fn language() -> String {
    std::env::var("LANG")
        .ok()
        .filter(|l| l.starts_with("ja"))
        .map_or_else(|| "en".into(), |_| "ja".into())
}

impl App {
    fn so2r_view(&self) -> So2rView {
        So2rView {
            link: LinkState::Connected,
            set_up: true,
            problem: self.error.clone(),
            rx: self.operator.rx(),
            ptt: self.ptt,
            tx: self.operator.on_air().unwrap_or(self.operator.focus()),
            message: true,
            ..So2rView::default()
        }
    }

    fn rig_action(&mut self, a: RigAction) -> Vec<TxCommand> {
        let n = usize::from(self.operator.focus() == 2);
        let now = Instant::now();
        match a {
            RigAction::Select(r) => return self.operator.set_focus(r),
            RigAction::Tune(hz) => self.rigs[n].1.send(Command::SetFreq(Vfo::A, hz), now),
            RigAction::Mode(m) => self.rigs[n].1.send(Command::SetMode(Vfo::A, m), now),
            _ => {}
        }
        Vec::new()
    }

    fn so2r_action(&mut self, a: So2rAction) -> Vec<TxCommand> {
        match a {
            So2rAction::Rx(Rx::Both) => self.operator.set_rx(RxMode::Both),
            So2rAction::Rx(rx) if rx == Rx::of(self.operator.focus()) => {
                self.operator.set_rx(RxMode::Focus)
            }
            So2rAction::Rx(_) | So2rAction::ToggleTx => self.operator.toggle_focus(),
            So2rAction::Send(text) => vec![self.operator.send(Signal::Cw(text))],
            So2rAction::Stop => {
                self.operator.stop_alternate();
                vec![TxCommand::Stop]
            }
            _ => Vec::new(),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let mut commands = Vec::new();
        for e in self.events.try_iter().collect::<Vec<_>>() {
            match &e {
                TxEvent::Error(m) => self.error = m.clone(),
                TxEvent::Started { .. } => self.ptt = true,
                TxEvent::Finished { .. } | TxEvent::Stopped => self.ptt = false,
                _ => {}
            }
            commands.extend(self.operator.on_event(&e, now));
        }
        commands.extend(self.operator.tick(now));
        for (_, r) in &mut self.rigs {
            r.tick(now);
        }
        let n = usize::from(self.operator.focus() == 2);
        let mut view = RigView::of(self.rigs[n].1.as_ref(), &self.rigs[n].0);
        view.number = self.operator.focus();
        view.rigs = self.rigs.len() as u8;
        let so2r = self.so2r_view();
        let mut rig_actions = Vec::new();
        let mut so2r_actions = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            rig_actions = self.panel.ui(ui, &view);
            ui.separator();
            so2r_actions = self.so2r.ui(ui, &so2r);
        });
        for a in rig_actions {
            commands.extend(self.rig_action(a));
        }
        for a in so2r_actions {
            commands.extend(self.so2r_action(a));
        }
        for c in commands {
            (self.send)(c);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
}

fn main() -> eframe::Result {
    let app = local();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([320.0, 640.0])
            .with_min_inner_size([280.0, 400.0])
            .with_title("HaML"),
        ..Default::default()
    };
    eframe::run_native(
        "HaML",
        options,
        Box::new(|cc| {
            icon::install(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}
