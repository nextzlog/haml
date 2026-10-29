use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haml::ptt::LinePtt;
use haml::remote::{Client, Server, Station};
use haml::rig::RigDriver;
use haml::sim::{Impairments, SimKeyer, SimLines, SimSo2r, Simulator};
use haml::tx::{Delays, Signal};
use haml::{
    Arbiter, Cat, Command, Config, Family, Keyer, KeyerEvent, Line, Mode, PortInfo, Ptt, Rig, So2r,
    Status, Timing, TxCommand, TxEvent, Vfo,
};

const SECRET: &str = "73 de remote";

const CAPS: haml::RigCaps = haml::RigCaps {
    xit: false,
    transceive: false,
    ..haml::RigCaps::ALL
};

fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Bench {
    server: Server,
    sim: Simulator,
    lines: SimLines,
    sent: Arc<Mutex<Vec<String>>>,
    boxed: Arc<Mutex<Vec<haml::sim::so2r::SimCommand>>>,
}

fn bench() -> Bench {
    let family = Family::Icom { address: 0x94 };
    let sim = Simulator::new(family, Impairments::clean(), 7);
    sim.realtime(true);
    let config = Config {
        interval: Duration::from_millis(5),
        ..Config::default()
    };
    let rig = RigDriver::spawn(Cat::new(family.protocol(), sim.transport(), config));
    let keyer = SimKeyer::new(Timing {
        wpm: 60,
        ..Timing::default()
    });
    let sent = keyer.sent.clone();
    let lines = SimLines::new(Line::Dtr, Timing::default());
    let boxed = Arc::new(Mutex::new(Vec::new()));
    let tx = Arbiter::new(2, Delays::default())
        .with_keyer(Box::new(keyer))
        .with_so2r(Box::new(SimSo2r::new(boxed.clone())))
        .with_ptt(2, Box::new(LinePtt::new(lines.clone(), Line::Rts)))
        .spawn();
    let station = Station {
        rigs: vec![("IC-7300".into(), CAPS, rig)],
        tx,
        tx_devices: vec!["simulated keyer".into(), "simulated SO2R box".into()],
        ports: Some(Box::new(|| Ok(vec![PortInfo::named("COM3")]))),
        lent: None,
    };
    let server = Server::start("127.0.0.1:0", SECRET, station).unwrap();
    Bench {
        server,
        sim,
        lines,
        sent,
        boxed,
    }
}

#[test]
fn a_wrong_secret_is_refused() {
    let b = bench();
    let e = Client::connect(b.server.addr(), "guess").err().unwrap();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn the_client_lists_and_drives_the_server_devices() {
    let b = bench();
    let client = Client::connect(b.server.addr(), SECRET).unwrap();
    let listing = client.list(Duration::from_secs(5)).unwrap();
    assert_eq!(listing.rigs, [("IC-7300".to_string(), CAPS)]);
    assert_eq!(listing.tx, ["simulated keyer", "simulated SO2R box"]);
    assert_eq!(listing.ports, [PortInfo::named("COM3")]);

    let mut rig = client.rig(0);
    assert_eq!(rig.caps(), CAPS, "the caps of the server's rig, not all");
    assert_eq!(client.rig(5).caps(), haml::RigCaps::NONE);
    rig.send(Command::SetFreq(Vfo::A, 14_025_000), Instant::now());
    rig.send(Command::SetMode(Vfo::A, Mode::Cw), Instant::now());
    let mut done = Vec::new();
    until("the rig answered", || {
        rig.tick(Instant::now());
        done.extend(rig.take_outcomes());
        done.len() == 2
    });
    assert!(done.iter().all(|o| o.status == Status::Done), "{done:?}");
    assert_eq!(b.sim.radio().freq(), 14_025_000);
    until("the state came back", || {
        rig.tick(Instant::now());
        rig.state().freq() == Some(14_025_000) && rig.state().mode == Some(Mode::Cw)
    });

    let mut so2r = client.so2r();
    so2r.select_rig(2);
    let events = client.tx_events();
    let mut keyer = client.keyer_on_focus();
    keyer.send("TEST");
    assert!(keyer.busy());
    until("keyed on the server", || {
        keyer.poll().contains(&KeyerEvent::Finished)
    });
    assert_eq!(*b.sent.lock().unwrap(), ["TEST"]);
    assert!(
        events
            .try_iter()
            .any(|e| matches!(e, TxEvent::Started { radio: 2, .. })),
        "the keyer follows the radio the box was told"
    );
    assert!(b
        .boxed
        .lock()
        .unwrap()
        .contains(&haml::sim::so2r::SimCommand::Tx(2)));
}

#[test]
fn losing_the_client_stops_all_tx_on_the_server() {
    let b = bench();
    let client = Client::connect(b.server.addr(), SECRET).unwrap();
    let events = client.tx_events();
    client
        .send(&TxCommand::Send {
            radio: 2,
            signal: Signal::Voice,
        })
        .unwrap();
    until("PTT on", || b.lines.state().ptt);
    until("started", || {
        events
            .try_iter()
            .any(|e| matches!(e, TxEvent::Started { radio: 2, .. }))
    });
    assert_eq!(b.server.peers().len(), 1);
    client.close();
    until("PTT released", || !b.lines.state().ptt);
    until("the peer is gone", || b.server.peers().is_empty());
}

#[test]
fn an_emergency_stop_from_the_client_releases_ptt() {
    let b = bench();
    let client = Client::connect(b.server.addr(), SECRET).unwrap();
    let mut ptt = client.ptt(2);
    ptt.set_ptt(true).unwrap();
    until("PTT on", || b.lines.state().ptt);
    client.send(&TxCommand::Stop).unwrap();
    until("PTT released", || !b.lines.state().ptt);
    assert!(client.connected());
}

#[test]
fn a_change_of_what_is_lent_reaches_the_client() {
    let family = Family::Icom { address: 0x94 };
    let sim = Simulator::new(family, Impairments::clean(), 7);
    let rig = RigDriver::spawn(Cat::new(
        family.protocol(),
        sim.transport(),
        Config::default(),
    ));
    let lent = Arc::new(Mutex::new(haml::remote::Lent::default()));
    let shown = lent.clone();
    let station = Station {
        rigs: vec![("RIG1".into(), haml::RigCaps::NONE, rig)],
        tx: Arbiter::new(2, Delays::default()).spawn(),
        tx_devices: Vec::new(),
        ports: None,
        lent: Some(Box::new(move || shown.lock().unwrap().clone())),
    };
    let server = Server::start("127.0.0.1:0", SECRET, station).unwrap();
    let client = Client::connect(server.addr(), SECRET).unwrap();
    let first = client.list(Duration::from_secs(5)).unwrap();
    assert_eq!(first.rigs, [("RIG1".to_string(), haml::RigCaps::NONE)]);
    assert!(!client.changed());
    *lent.lock().unwrap() = haml::remote::Lent {
        rigs: vec![("RIG1 IC-7300".into(), CAPS)],
        tx: vec!["keyer".into()],
    };
    until("told of the change", || client.changed());
    let again = client.list(Duration::from_secs(5)).unwrap();
    assert_eq!(again.rigs, [("RIG1 IC-7300".to_string(), CAPS)]);
    assert_eq!(again.tx, ["keyer"]);
    assert_eq!(client.rig(0).caps(), CAPS);
}
