#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::any::Any;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haml::remote::{Client, Server, Station};
use haml::rig::{RigClient, RigDriver};
use haml::sim::{Impairments, SimKeyer, SimSo2r, Simulator};
use haml::tx::Delays;
use haml::{Arbiter, Cat, Command, Config, Device, Family, RigCaps, RigEvent, Timing};
use haml::{TxCommand, TxEvent};
use haml_ui::{icon, RigPanel, So2rPanel};

type Sender = Box<dyn Fn(TxCommand)>;

struct App {
    rigs: Vec<RigPanel>,
    so2r: So2rPanel,
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

fn connect(addr: &str, secret: &str) -> Result<App, String> {
    let client = Client::connect(addr, secret).map_err(|e| e.to_string())?;
    let listing = client
        .list(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    let rigs = listing
        .rigs
        .iter()
        .enumerate()
        .map(|(i, (name, _))| RigPanel::new(name, Box::new(client.rig(i))))
        .collect();
    let events = client.tx_events();
    let c = client.clone();
    Ok(App {
        rigs,
        so2r: So2rPanel::default(),
        send: Box::new(move |command| {
            let _ = c.send(&command);
        }),
        events,
        _keep: vec![Box::new(client)],
    })
}

fn local(serve: Option<(&str, &str)>) -> Result<App, String> {
    let mut keep: Vec<Box<dyn Any>> = Vec::new();
    let devices = simulated(&mut keep);
    let rigs = devices
        .iter()
        .map(|(name, d)| RigPanel::new(name, Box::new(RigClient::new(d, RigCaps::ALL))))
        .collect();
    let tx = Arbiter::new(2, Delays::default())
        .with_keyer(Box::new(SimKeyer::new(Timing::default())))
        .with_so2r(Box::new(SimSo2r::new(Arc::new(Mutex::new(Vec::new())))))
        .spawn();
    let events = tx.subscribe();
    let commands = tx.commander();
    match serve {
        Some((addr, secret)) => {
            let station = Station {
                rigs: devices
                    .into_iter()
                    .map(|(name, d)| (name, RigCaps::ALL, d))
                    .collect(),
                tx,
                tx_devices: vec!["simulated keyer".into(), "simulated SO2R box".into()],
                ports: None,
                lent: None,
            };
            let server = Server::start(addr, secret, station).map_err(|e| e.to_string())?;
            keep.push(Box::new(server));
        }
        None => {
            keep.push(Box::new(devices));
            keep.push(Box::new(tx));
        }
    }
    Ok(App {
        rigs,
        so2r: So2rPanel::default(),
        send: Box::new(move |command| {
            let _ = commands.send(command);
        }),
        events,
        _keep: keep,
    })
}

impl App {
    fn new(args: &[String]) -> Result<Self, String> {
        match args {
            [flag, addr, secret] if flag == "--connect" => connect(addr, secret),
            [flag, addr, secret] if flag == "--serve" => local(Some((addr, secret))),
            [] => local(None),
            _ => Err("usage: haml [--connect HOST:PORT SECRET | --serve ADDR:PORT SECRET]".into()),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        for e in self.events.try_iter() {
            self.so2r.on_event(&e, now);
        }
        for c in self.so2r.take_commands(now) {
            (self.send)(c);
        }
        for r in &mut self.rigs {
            r.tick(now);
        }
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| self.so2r.toolbar_ui(ui));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.columns(self.rigs.len().max(1), |cols| {
                for (col, rig) in cols.iter_mut().zip(&mut self.rigs) {
                    rig.ui(col);
                }
            });
            ui.separator();
            self.so2r.ui(ui);
        });
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let app = match App::new(&args) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 420.0])
            .with_min_inner_size([320.0, 280.0])
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
