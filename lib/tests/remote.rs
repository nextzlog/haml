use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haml::ptt::LinePtt;
use haml::remote::{
    pair, AuditKind, Client, ConnectError, Limits, LoopbackListener, Server, Station, TcpConn,
};
use haml::rig::RigDriver;
use haml::sim::{Impairments, SimKeyer, SimLines, SimSo2r, Simulator};
use haml::tx::{Delays, Signal};
use haml::{
    Arbiter, Cat, Command, Config, Family, Keyer, KeyerEvent, Line, Mode, PortInfo, Ptt, Rig, So2r,
    Status, Timing, TxCommand, TxEvent, Vfo,
};

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

impl Bench {
    fn connect(&self) -> Result<Client, ConnectError> {
        self.connect_with(Some(haml::remote::KEEPALIVE))
    }

    fn connect_with(&self, keepalive: Option<Duration>) -> Result<Client, ConnectError> {
        let (near, far) = pair("client", "server");
        self.server.attach(Box::new(far));
        Client::attach_with(Box::new(near), keepalive)
    }
}

fn bench() -> Bench {
    bench_with(Limits::default())
}

fn bench_with(limits: Limits) -> Bench {
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
        channels: Vec::new(),
    };
    Bench {
        server: Server::new(station, limits),
        sim,
        lines,
        sent,
        boxed,
    }
}

fn empty_station() -> Station {
    Station {
        rigs: Vec::new(),
        tx: Arbiter::new(2, Delays::default()).spawn(),
        tx_devices: Vec::new(),
        ports: None,
        lent: None,
        channels: Vec::new(),
    }
}

#[test]
fn a_loopback_tcp_listener_serves_a_client() {
    let listener = LoopbackListener::bind(0).unwrap();
    let port = listener.port().unwrap();
    let server = Server::start(listener, empty_station(), Limits::default()).unwrap();
    let client = Client::attach(Box::new(TcpConn::loopback(port).unwrap())).unwrap();
    let listing = client.list(Duration::from_secs(5)).unwrap();
    assert!(listing.rigs.is_empty());
    assert_eq!(server.peers().len(), 1);
    assert!(server.peers()[0].starts_with("127.0.0.1:"));
    client.close();
    until("the peer is gone", || server.peers().is_empty());
}

#[test]
fn an_in_process_listener_serves_a_client() {
    let (accept, listener) = std::sync::mpsc::channel::<Box<dyn haml::remote::Conn>>();
    let server = Server::start(listener, empty_station(), Limits::default()).unwrap();
    let (near, far) = pair("me", "you");
    accept.send(Box::new(far)).unwrap();
    let client = Client::attach(Box::new(near)).unwrap();
    assert!(client.list(Duration::from_secs(5)).is_ok());
    assert_eq!(server.peers(), ["me"]);
}

#[test]
fn a_kick_drops_the_session() {
    let b = bench();
    let audit = b.server.audit();
    let client = b.connect().unwrap();
    b.server.kick();
    until("the session is dropped", || !client.connected());
    assert!(audit.try_iter().any(|a| a.kind == AuditKind::Kicked));
    assert!(b.connect().is_ok());
}

#[test]
fn the_client_lists_and_drives_the_server_devices() {
    let b = bench();
    let client = b.connect().unwrap();
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
    let client = b.connect().unwrap();
    let events = client.tx_events();
    client
        .send(&TxCommand::Send {
            id: haml::ticket(),
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
    let client = b.connect().unwrap();
    let mut ptt = client.ptt(2);
    ptt.set_ptt(true).unwrap();
    until("PTT on", || b.lines.state().ptt);
    let audit = b.server.audit();
    client.send(&TxCommand::Stop).unwrap();
    until("PTT released", || !b.lines.state().ptt);
    until("audited", || {
        audit.try_iter().any(|a| a.kind == AuditKind::EmergencyStop)
    });
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
        channels: Vec::new(),
    };
    let server = Server::new(station, Limits::default());
    let (near, far) = pair("client", "server");
    server.attach(Box::new(far));
    let client = Client::attach(Box::new(near)).unwrap();
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

#[test]
fn only_one_client_controls_the_station() {
    let b = bench();
    let audit = b.server.audit();
    let first = b.connect().unwrap();
    assert!(matches!(b.connect(), Err(ConnectError::Busy)));
    assert_eq!(b.server.peers().len(), 1);
    assert!(first.connected());
    first.close();
    until("the slot is free", || b.server.peers().is_empty());
    assert!(b.connect().is_ok());
    assert!(audit.try_iter().any(|a| a.kind == AuditKind::Busy));
}

#[test]
fn an_idle_client_is_dropped_and_its_tx_stopped() {
    let b = bench_with(Limits {
        idle: Duration::from_millis(300),
    });
    let audit = b.server.audit();
    let client = b.connect_with(None).unwrap();
    let mut ptt = client.ptt(2);
    ptt.set_ptt(true).unwrap();
    until("PTT on", || b.lines.state().ptt);
    until("PTT released", || !b.lines.state().ptt);
    until("disconnected", || !client.connected());
    let kinds: Vec<_> = audit.try_iter().map(|a| a.kind).collect();
    assert!(kinds.contains(&AuditKind::IdleTimeout), "{kinds:?}");
    assert!(
        kinds.contains(&AuditKind::TxStarted { radio: 2 }),
        "{kinds:?}"
    );
}

#[test]
fn keepalive_pings_hold_an_idle_session() {
    let b = bench_with(Limits {
        idle: Duration::from_millis(300),
    });
    let client = b.connect_with(Some(Duration::from_millis(50))).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert!(client.connected());
}

#[test]
fn a_channel_carries_lines_both_ways_and_hears_the_session_end() {
    use haml::remote::{Channel, Inbound};
    let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let w = woken.clone();
    let modem = Channel::new("rtty").waking(move || {
        w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let station = Station {
        channels: vec![Channel::new("ft8"), modem.clone()],
        ..empty_station()
    };
    let server = Server::new(station, Limits::default());
    let (near, far) = pair("client", "server");
    server.attach(Box::new(far));
    let client = Client::attach(Box::new(near)).unwrap();
    let listing = client.list(Duration::from_secs(5)).unwrap();
    assert_eq!(listing.channels, ["ft8", "rtty"]);
    let ch = client.channel(1);
    ch.send("START\nnow").unwrap();
    let mut got = Vec::new();
    until("the server hears the line", || {
        got.extend(modem.take());
        got.len() >= 2
    });
    assert_eq!(
        got,
        [
            Inbound::Open("client".into()),
            Inbound::Line("START now".into())
        ]
    );
    assert!(
        woken.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "each line wakes the station"
    );
    modem.say("DECODE CQ TEST");
    let mut heard = Vec::new();
    until("the client hears the reply", || {
        heard.extend(ch.recv());
        !heard.is_empty()
    });
    assert_eq!(heard, ["DECODE CQ TEST"]);
    assert!(client.channel(0).recv().is_empty());
    client.close();
    until("the server sees the end", || {
        modem.take().contains(&Inbound::Closed)
    });
    assert!(ch.send("x").is_err() || !ch.connected());
}
