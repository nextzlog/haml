pub mod wire;

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::device::Device;
use crate::keyer::{Keyer, KeyerCaps, KeyerEvent};
use crate::ports::{Link, PortInfo, Scan};
use crate::ptt::Ptt;
use crate::so2r::{Rx, So2r, So2rCaps};
use crate::tx::{Signal, TxCommand, TxEvent};
use crate::{Command, Error, Event, Outcome, Rig, RigCaps, RigEvent, State};

pub const HELLO: &str = "HELLO";
pub const WELCOME: &str = "WELCOME";
pub const DENIED: &str = "DENIED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub rigs: Vec<(String, RigCaps)>,
    pub tx: Vec<String>,
    pub ports: Vec<PortInfo>,
}

type PortLister = Box<dyn Fn() -> Scan + Send + Sync>;

pub const CHANGED: &str = "CHANGED";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lent {
    pub rigs: Vec<(String, RigCaps)>,
    pub tx: Vec<String>,
}

type Lender = Box<dyn Fn() -> Lent + Send + Sync>;

pub struct Station {
    pub rigs: Vec<(String, RigCaps, Device<Command, RigEvent>)>,
    pub tx: Device<TxCommand, TxEvent>,
    pub tx_devices: Vec<String>,
    pub ports: Option<PortLister>,
    pub lent: Option<Lender>,
}

impl Station {
    fn lent(&self) -> Lent {
        let fixed = Lent {
            rigs: self.rigs.iter().map(|(n, c, _)| (n.clone(), *c)).collect(),
            tx: self.tx_devices.clone(),
        };
        let mut now = self.lent.as_ref().map_or_else(|| fixed.clone(), |f| f());
        now.rigs
            .resize(self.rigs.len(), (String::new(), RigCaps::NONE));
        for (r, f) in now.rigs.iter_mut().zip(fixed.rigs) {
            if r.0.is_empty() {
                *r = f;
            }
        }
        now
    }

    fn listing(&self) -> Vec<String> {
        let lent = self.lent();
        let mut lines: Vec<String> = lent
            .rigs
            .iter()
            .enumerate()
            .map(|(i, (name, caps))| {
                format!("DEVICE rig {i} {} {}", wire::caps(caps), wire::clean(name))
            })
            .collect();
        lines.extend(
            lent.tx
                .iter()
                .map(|d| format!("DEVICE tx 0 {}", wire::clean(d))),
        );
        if let Some(Ok(ports)) = self.ports.as_ref().map(|f| f()) {
            for p in ports {
                lines.push(format!(
                    "PORT {}\t{}",
                    wire::clean(&p.name),
                    wire::clean(&p.detail())
                ));
            }
        }
        lines.push("END".into());
        lines
    }
}

pub struct Server {
    addr: SocketAddr,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(addr: impl ToSocketAddrs, secret: &str, station: Station) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let station = Arc::new(station);
        let secret = secret.to_string();
        let halt = stop.clone();
        let peers: Arc<Mutex<Vec<SocketAddr>>> = Arc::default();
        let sessions = peers.clone();
        let thread = std::thread::Builder::new()
            .name("haml-server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if halt.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (station, secret, halt) = (station.clone(), secret.clone(), halt.clone());
                    let peers = sessions.clone();
                    let _ = std::thread::Builder::new()
                        .name("haml-session".into())
                        .spawn(move || session(stream, &secret, &station, &halt, &peers));
                }
                let _ = station.tx.send(TxCommand::Stop);
            })?;
        Ok(Server {
            addr,
            peers,
            stop,
            thread: Some(thread),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn peers(&self) -> Vec<SocketAddr> {
        self.peers.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type Peers = Mutex<Vec<SocketAddr>>;

fn session(stream: TcpStream, secret: &str, station: &Station, halt: &AtomicBool, peers: &Peers) {
    let _ = stream.set_nodelay(true);
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut lines = BufReader::new(stream);
    let mut hello = String::new();
    let _ = lines
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)));
    if lines.read_line(&mut hello).is_err() || hello.trim_end() != format!("{HELLO} {secret}") {
        let _ = writeln!(out, "{DENIED}");
        return;
    }
    if writeln!(out, "{WELCOME}").is_err() {
        return;
    }
    let peer = out.peer_addr().ok();
    if let Some(p) = peer {
        peers.lock().unwrap_or_else(|e| e.into_inner()).push(p);
    }
    let rigs: Vec<_> = station.rigs.iter().map(|(_, _, d)| d.subscribe()).collect();
    let tx = station.tx.subscribe();
    let _ = lines
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(10)));
    let mut line = String::new();
    let mut told = station.lent();
    while !halt.load(Ordering::SeqCst) {
        let mut text = String::new();
        let now = station.lent();
        if now != told {
            told = now;
            text.push_str(CHANGED);
            text.push('\n');
        }
        for (i, r) in rigs.iter().enumerate() {
            for e in r.try_iter() {
                text.push_str(&format!("RIG {i} {}\n", wire::rig_event(&e)));
            }
        }
        for e in tx.try_iter() {
            text.push_str(&format!("TX {}\n", wire::tx_event(&e)));
        }
        if !text.is_empty() && out.write_all(text.as_bytes()).is_err() {
            break;
        }
        match lines.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.ends_with('\n') => {
                let request = std::mem::take(&mut line);
                if !serve(request.trim_end(), station, &mut out) {
                    break;
                }
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    if let Some(p) = peer {
        peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|q| *q != p);
    }
    let _ = station.tx.send(TxCommand::Stop);
    let _ = out.shutdown(Shutdown::Both);
}

fn serve(request: &str, station: &Station, out: &mut TcpStream) -> bool {
    let (head, rest) = request.split_once(' ').unwrap_or((request, ""));
    match head {
        "LIST" => {
            let text: String = station.listing().into_iter().map(|l| l + "\n").collect();
            out.write_all(text.as_bytes()).is_ok()
        }
        "RIG" => {
            let (i, c) = rest.split_once(' ').unwrap_or((rest, ""));
            let rig = i.parse::<usize>().ok().and_then(|i| station.rigs.get(i));
            match (rig, wire::parse_command(c)) {
                (Some((_, _, d)), Some(c)) => {
                    let _ = d.send(c);
                    true
                }
                _ => writeln!(out, "TX error bad request {}", wire::clean(request)).is_ok(),
            }
        }
        "TX" => match wire::parse_tx_command(rest) {
            Some(c) => {
                let _ = station.tx.send(c);
                true
            }
            None => writeln!(out, "TX error bad request {}", wire::clean(request)).is_ok(),
        },
        "BYE" => false,
        _ => writeln!(out, "TX error bad request {}", wire::clean(request)).is_ok(),
    }
}

type Fanout<E> = Arc<Mutex<Vec<Sender<E>>>>;

fn fan<E: Clone>(subs: &Fanout<E>, e: E) {
    subs.lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|s| s.send(e.clone()).is_ok());
}

fn listen<E>(subs: &Fanout<E>) -> Receiver<E> {
    let (tx, rx) = mpsc::channel();
    subs.lock().unwrap_or_else(|e| e.into_inner()).push(tx);
    rx
}

#[derive(Default)]
struct Routes {
    rigs: Mutex<Vec<Fanout<RigEvent>>>,
    tx: Fanout<TxEvent>,
    listing: Fanout<String>,
    caps: Mutex<Vec<RigCaps>>,
    changed: AtomicBool,
    focus: AtomicU8,
    open: AtomicBool,
}

impl Routes {
    fn rig(&self, i: usize) -> Fanout<RigEvent> {
        let mut r = self.rigs.lock().unwrap_or_else(|e| e.into_inner());
        while r.len() <= i {
            r.push(Fanout::default());
        }
        r[i].clone()
    }
}

#[derive(Clone)]
pub struct Client {
    out: Arc<Mutex<TcpStream>>,
    routes: Arc<Routes>,
}

impl Client {
    pub fn connect(addr: impl ToSocketAddrs, secret: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        let mut out = stream.try_clone()?;
        writeln!(out, "{HELLO} {}", wire::clean(secret))?;
        let mut reader = BufReader::new(stream);
        let mut answer = String::new();
        reader.read_line(&mut answer)?;
        if answer.trim_end() != WELCOME {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the server refused the secret",
            ));
        }
        let routes = Arc::new(Routes::default());
        routes.open.store(true, Ordering::SeqCst);
        routes.focus.store(1, Ordering::SeqCst);
        let r = routes.clone();
        std::thread::Builder::new()
            .name("haml-client".into())
            .spawn(move || {
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    route(&r, &line);
                }
                r.open.store(false, Ordering::SeqCst);
                let rigs = r.rigs.lock().unwrap_or_else(|e| e.into_inner()).clone();
                for rig in rigs {
                    fan(&rig, RigEvent::Link(Link::Disconnected));
                }
                fan(&r.tx, TxEvent::Error("the server is gone".into()));
            })?;
        Ok(Client {
            out: Arc::new(Mutex::new(out)),
            routes,
        })
    }

    pub fn connected(&self) -> bool {
        self.routes.open.load(Ordering::SeqCst)
    }

    pub fn changed(&self) -> bool {
        self.routes.changed.swap(false, Ordering::SeqCst)
    }

    fn line(&self, text: &str) -> crate::Result<()> {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(out, "{text}").map_err(|_| Error::Disconnected)
    }

    pub fn list(&self, timeout: Duration) -> crate::Result<Listing> {
        let rx = listen(&self.routes.listing);
        self.line("LIST")?;
        let mut listing = Listing {
            rigs: Vec::new(),
            tx: Vec::new(),
            ports: Vec::new(),
        };
        let start = Instant::now();
        loop {
            let left = timeout.checked_sub(start.elapsed()).ok_or(Error::Timeout)?;
            let line = rx.recv_timeout(left).map_err(|_| Error::Timeout)?;
            if line == "END" {
                *self.routes.caps.lock().unwrap_or_else(|e| e.into_inner()) =
                    listing.rigs.iter().map(|r| r.1).collect();
                return Ok(listing);
            }
            if let Some(rest) = line.strip_prefix("DEVICE rig ") {
                let mut w = rest.splitn(3, ' ');
                let (_, caps, name) = (w.next(), w.next(), w.next().unwrap_or(""));
                let caps = caps.and_then(wire::parse_caps).unwrap_or(RigCaps::NONE);
                listing.rigs.push((name.into(), caps));
            } else if let Some(rest) = line.strip_prefix("DEVICE tx 0 ") {
                listing.tx.push(rest.into());
            } else if let Some(rest) = line.strip_prefix("PORT ") {
                let (name, detail) = rest.split_once('\t').unwrap_or((rest, ""));
                listing.ports.push(PortInfo {
                    description: detail.into(),
                    ..PortInfo::named(name)
                });
            }
        }
    }

    pub fn send(&self, command: &TxCommand) -> crate::Result<()> {
        self.line(&format!("TX {}", wire::tx_command(command)))
    }

    pub fn tx_events(&self) -> Receiver<TxEvent> {
        listen(&self.routes.tx)
    }

    pub fn rig(&self, index: usize) -> RemoteRig {
        let caps = self.routes.caps.lock().unwrap_or_else(|e| e.into_inner());
        RemoteRig {
            caps: caps.get(index).copied().unwrap_or(RigCaps::NONE),
            client: self.clone(),
            index,
            events: listen(&self.routes.rig(index)),
            state: State::default(),
            outcomes: Vec::new(),
            link: Link::Connected,
        }
    }

    pub fn keyer(&self, radio: u8) -> RemoteKeyer {
        RemoteKeyer {
            radio: Some(radio),
            ..self.keyer_on_focus()
        }
    }

    pub fn keyer_on_focus(&self) -> RemoteKeyer {
        RemoteKeyer {
            client: self.clone(),
            radio: None,
            events: self.tx_events(),
            wpm: 25,
            pending: 0,
        }
    }

    pub fn so2r(&self) -> RemoteSo2r {
        RemoteSo2r(self.clone())
    }

    pub fn ptt(&self, radio: u8) -> RemotePtt {
        RemotePtt {
            client: self.clone(),
            radio,
            on: false,
        }
    }

    pub fn close(&self) {
        let _ = self.line("BYE");
        let out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = out.shutdown(Shutdown::Both);
    }
}

fn route(r: &Routes, line: &str) {
    let (head, rest) = line.split_once(' ').unwrap_or((line, ""));
    match head {
        "RIG" => {
            let (i, e) = rest.split_once(' ').unwrap_or((rest, ""));
            if let (Ok(i), Some(e)) = (i.parse::<usize>(), wire::parse_rig_event(e)) {
                fan(&r.rig(i), e);
            }
        }
        "TX" => {
            if let Some(e) = wire::parse_tx_event(rest) {
                fan(&r.tx, e);
            }
        }
        CHANGED => r.changed.store(true, Ordering::SeqCst),
        _ => fan(&r.listing, line.to_string()),
    }
}

pub struct RemoteRig {
    caps: RigCaps,
    client: Client,
    index: usize,
    events: Receiver<RigEvent>,
    state: State,
    outcomes: Vec<Outcome>,
    link: Link,
}

impl Rig for RemoteRig {
    fn caps(&self) -> RigCaps {
        self.caps
    }

    fn send(&mut self, command: Command, _now: Instant) {
        let line = format!("RIG {} {}", self.index, wire::command(&command));
        if self.client.line(&line).is_err() {
            self.link = Link::Disconnected;
        }
    }

    fn tick(&mut self, _now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        for e in self.events.try_iter() {
            match e {
                RigEvent::Event(e) => {
                    self.state.apply(e);
                    events.push(e);
                }
                RigEvent::Outcome(o) => self.outcomes.push(o),
                RigEvent::Link(l) => self.link = l,
            }
        }
        events
    }

    fn take_outcomes(&mut self) -> Vec<Outcome> {
        std::mem::take(&mut self.outcomes)
    }

    fn state(&self) -> State {
        self.state
    }

    fn link(&self) -> Link {
        self.link
    }
}

pub struct RemoteKeyer {
    client: Client,
    radio: Option<u8>,
    events: Receiver<TxEvent>,
    wpm: u32,
    pending: usize,
}

impl RemoteKeyer {
    fn radio(&self) -> u8 {
        self.radio
            .unwrap_or_else(|| self.client.routes.focus.load(Ordering::SeqCst))
    }
}

impl Keyer for RemoteKeyer {
    fn caps(&self) -> KeyerCaps {
        KeyerCaps {
            ptt: true,
            ptt_delays: true,
            ..KeyerCaps::default()
        }
    }

    fn send(&mut self, text: &str) {
        self.pending += 1;
        let _ = self.client.send(&TxCommand::Send {
            radio: self.radio(),
            signal: Signal::Cw(text.into()),
        });
    }

    fn abort(&mut self) {
        let _ = self.client.send(&TxCommand::Stop);
    }

    fn set_wpm(&mut self, wpm: u32) {
        self.wpm = wpm;
        let _ = self.client.send(&TxCommand::SetWpm(wpm));
    }

    fn wpm(&self) -> u32 {
        self.wpm
    }

    fn set_ptt(&mut self, on: bool) {
        let c = if on {
            TxCommand::Send {
                radio: self.radio(),
                signal: Signal::Voice,
            }
        } else {
            TxCommand::Release
        };
        let _ = self.client.send(&c);
    }

    fn busy(&self) -> bool {
        self.pending > 0
    }

    fn poll(&mut self) -> Vec<KeyerEvent> {
        let mut out = Vec::new();
        for e in self.events.try_iter() {
            match e {
                TxEvent::Finished { .. } if self.pending > 0 => {
                    self.pending -= 1;
                    if self.pending == 0 {
                        out.push(KeyerEvent::Finished);
                    }
                }
                TxEvent::Stopped if self.pending > 0 => {
                    self.pending = 0;
                    out.push(KeyerEvent::Finished);
                }
                TxEvent::Keyer(KeyerEvent::Finished) => {}
                TxEvent::Keyer(e) => out.push(e),
                TxEvent::Error(e) => out.push(KeyerEvent::Error(e)),
                _ => {}
            }
        }
        out
    }
}

pub struct RemoteSo2r(Client);

impl So2r for RemoteSo2r {
    fn caps(&self) -> So2rCaps {
        So2rCaps {
            rx: true,
            rx_both: true,
            blend: true,
            auto_rx: false,
        }
    }

    fn tx(&mut self, radio: u8) {
        self.0.routes.focus.store(radio, Ordering::SeqCst);
        let _ = self.0.send(&TxCommand::Transmit(radio));
    }

    fn rx(&mut self, rx: Rx) {
        let _ = self.0.send(&TxCommand::Listen(rx));
    }

    fn select_rig(&mut self, radio: u8) {
        self.0.routes.focus.store(radio, Ordering::SeqCst);
        let _ = self.0.send(&TxCommand::Focus(radio));
        let _ = self.0.send(&TxCommand::Listen(Rx::of(radio)));
    }

    fn blend(&mut self, on: bool, percent: u8) {
        let _ = self.0.send(&TxCommand::Blend(on, percent));
    }

    fn error(&self) -> Option<String> {
        (!self.0.connected()).then(|| Error::Disconnected.to_string())
    }
}

pub struct RemotePtt {
    client: Client,
    radio: u8,
    on: bool,
}

impl Ptt for RemotePtt {
    fn set_ptt(&mut self, on: bool) -> crate::Result<()> {
        let c = if on {
            TxCommand::Send {
                radio: self.radio,
                signal: Signal::Voice,
            }
        } else {
            TxCommand::Release
        };
        self.client.send(&c)?;
        self.on = on;
        Ok(())
    }

    fn ptt(&self) -> bool {
        self.on
    }
}
