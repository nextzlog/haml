mod channel;
mod conn;
pub mod wire;

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::device::Device;
use crate::keyer::{Keyer, KeyerCaps, KeyerEvent};
use crate::ports::{Link, PortInfo, Scan};
use crate::ptt::Ptt;
use crate::so2r::{Rx, So2r, So2rCaps};
use crate::tx::{Signal, TxCommand, TxEvent};
use crate::{Command, Error, Event, Outcome, Rig, RigCaps, RigEvent, State};

pub use channel::{Channel, Inbound, RemoteChannel};
pub use conn::{pair, Conn, Listener, LoopbackListener, PairConn, TcpConn};

pub const WELCOME: &str = "WELCOME";
pub const BUSY: &str = "BUSY";
pub const PING: &str = "PING";
pub const KEEPALIVE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub rigs: Vec<(String, RigCaps)>,
    pub tx: Vec<String>,
    pub ports: Vec<PortInfo>,
    pub channels: Vec<String>,
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
    pub channels: Vec<Channel>,
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
        lines.extend(
            self.channels
                .iter()
                .enumerate()
                .map(|(i, c)| format!("DEVICE app {i} {}", c.name())),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub idle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            idle: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    Connected,
    Busy,
    TxStarted { radio: u8 },
    TxStopped { radio: Option<u8> },
    EmergencyStop,
    IdleTimeout,
    Kicked,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audit {
    pub time: SystemTime,
    pub peer: String,
    pub kind: AuditKind,
}

struct Shared {
    station: Station,
    limits: Limits,
    generation: AtomicU64,
    peers: Mutex<Vec<String>>,
    audit: Fanout<Audit>,
    halt: AtomicBool,
}

impl Shared {
    fn log(&self, peer: &str, kind: AuditKind) {
        fan(
            &self.audit,
            Audit {
                time: SystemTime::now(),
                peer: peer.into(),
                kind,
            },
        );
    }
}

pub struct Server {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

const POLL: Duration = Duration::from_millis(10);

impl Server {
    pub fn new(station: Station, limits: Limits) -> Self {
        Server {
            shared: Arc::new(Shared {
                station,
                limits,
                generation: AtomicU64::new(0),
                peers: Mutex::default(),
                audit: Fanout::default(),
                halt: AtomicBool::new(false),
            }),
            thread: None,
        }
    }

    pub fn start(
        mut listener: impl Listener + 'static,
        station: Station,
        limits: Limits,
    ) -> io::Result<Self> {
        let mut server = Server::new(station, limits);
        let s = server.shared.clone();
        server.thread = Some(
            std::thread::Builder::new()
                .name("haml-server".into())
                .spawn(move || {
                    while !s.halt.load(Ordering::SeqCst) {
                        match listener.accept(Duration::from_millis(50)) {
                            Ok(Some(conn)) => attach(&s, conn),
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                    let _ = s.station.tx.send(TxCommand::Stop);
                })?,
        );
        Ok(server)
    }

    pub fn attach(&self, conn: Box<dyn Conn>) {
        attach(&self.shared, conn);
    }

    pub fn peers(&self) -> Vec<String> {
        self.shared
            .peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn kick(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn audit(&self) -> Receiver<Audit> {
        listen(&self.shared.audit)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.halt.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = self.shared.station.tx.send(TxCommand::Stop);
    }
}

fn attach(s: &Arc<Shared>, conn: Box<dyn Conn>) {
    let s = s.clone();
    let _ = std::thread::Builder::new()
        .name("haml-session".into())
        .spawn(move || {
            let peer = conn.peer();
            s.log(&peer, AuditKind::Connected);
            session(conn, &peer, &s);
            s.log(&peer, AuditKind::Disconnected);
        });
}

fn session(mut conn: Box<dyn Conn>, peer: &str, s: &Shared) {
    {
        let mut peers = s.peers.lock().unwrap_or_else(|e| e.into_inner());
        if !peers.is_empty() {
            drop(peers);
            s.log(peer, AuditKind::Busy);
            let _ = conn.write_line(BUSY);
            return conn.shutdown();
        }
        peers.push(peer.into());
    }
    let generation = s.generation.load(Ordering::SeqCst);
    if conn.write_line(WELCOME).is_ok() {
        control(conn.as_mut(), peer, generation, s);
    }
    for c in &s.station.channels {
        c.push(Inbound::Closed);
    }
    s.peers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|q| q != peer);
    let _ = s.station.tx.send(TxCommand::Stop);
    conn.shutdown();
}

fn say(conn: &mut dyn Conn, text: &[String]) -> bool {
    text.iter().all(|l| conn.write_line(l).is_ok())
}

fn control(conn: &mut dyn Conn, peer: &str, generation: u64, s: &Shared) {
    let station = &s.station;
    let rigs: Vec<_> = station.rigs.iter().map(|(_, _, d)| d.subscribe()).collect();
    let tx = station.tx.subscribe();
    let apps: Vec<_> = station.channels.iter().map(Channel::listen).collect();
    for c in &station.channels {
        c.push(Inbound::Open(peer.into()));
    }
    let mut told = station.lent();
    let mut heard = Instant::now();
    while !s.halt.load(Ordering::SeqCst) {
        if s.generation.load(Ordering::SeqCst) != generation {
            s.log(peer, AuditKind::Kicked);
            return;
        }
        if heard.elapsed() > s.limits.idle {
            s.log(peer, AuditKind::IdleTimeout);
            return;
        }
        let mut text = Vec::new();
        let now = station.lent();
        if now != told {
            told = now;
            text.push(CHANGED.to_string());
        }
        for (i, r) in rigs.iter().enumerate() {
            text.extend(
                r.try_iter()
                    .map(|e| format!("RIG {i} {}", wire::rig_event(&e))),
            );
        }
        for (i, a) in apps.iter().enumerate() {
            text.extend(a.try_iter().map(|l| format!("APP {i} {l}")));
        }
        for e in tx.try_iter() {
            match e {
                TxEvent::Started { radio, .. } => s.log(peer, AuditKind::TxStarted { radio }),
                TxEvent::Finished { radio, .. } => {
                    s.log(peer, AuditKind::TxStopped { radio: Some(radio) })
                }
                TxEvent::Stopped => s.log(peer, AuditKind::TxStopped { radio: None }),
                _ => {}
            }
            text.push(format!("TX {}", wire::tx_event(&e)));
        }
        if !say(conn, &text) {
            return;
        }
        match conn.read_line(POLL) {
            Ok(Some(request)) => {
                heard = Instant::now();
                if !serve(request.trim_end(), station, conn, &|k| s.log(peer, k)) {
                    return;
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

fn serve(request: &str, station: &Station, out: &mut dyn Conn, log: &dyn Fn(AuditKind)) -> bool {
    let (head, rest) = request.split_once(' ').unwrap_or((request, ""));
    let bad = |out: &mut dyn Conn| {
        say(
            out,
            &[format!("TX error bad request {}", wire::clean(request))],
        )
    };
    match head {
        "LIST" => say(out, &station.listing()),
        "RIG" => {
            let (i, c) = rest.split_once(' ').unwrap_or((rest, ""));
            let rig = i.parse::<usize>().ok().and_then(|i| station.rigs.get(i));
            match (rig, wire::parse_command(c)) {
                (Some((_, _, d)), Some(c)) => {
                    let _ = d.send(c);
                    true
                }
                _ => bad(out),
            }
        }
        "TX" => match wire::parse_tx_command(rest) {
            Some(c) => {
                if c == TxCommand::Stop {
                    log(AuditKind::EmergencyStop);
                }
                let _ = station.tx.send(c);
                true
            }
            None => bad(out),
        },
        "APP" => {
            let (i, line) = rest.split_once(' ').unwrap_or((rest, ""));
            match i
                .parse::<usize>()
                .ok()
                .and_then(|i| station.channels.get(i))
            {
                Some(c) => {
                    c.push(Inbound::Line(line.into()));
                    true
                }
                None => bad(out),
            }
        }
        PING => true,
        "BYE" => false,
        _ => bad(out),
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
    apps: Mutex<Vec<Fanout<String>>>,
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

    fn app(&self, i: usize) -> Fanout<String> {
        let mut r = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        while r.len() <= i {
            r.push(Fanout::default());
        }
        r[i].clone()
    }
}

#[derive(Debug)]
pub enum ConnectError {
    Busy,
    Io(io::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Busy => f.write_str("another client controls the station"),
            ConnectError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<io::Error> for ConnectError {
    fn from(e: io::Error) -> Self {
        ConnectError::Io(e)
    }
}

enum Outgoing {
    Line(String),
    Close,
}

#[derive(Clone)]
pub struct Client {
    out: Sender<Outgoing>,
    routes: Arc<Routes>,
}

impl Client {
    pub fn attach(conn: Box<dyn Conn>) -> Result<Self, ConnectError> {
        Client::attach_with(conn, Some(KEEPALIVE))
    }

    pub fn attach_with(
        mut conn: Box<dyn Conn>,
        keepalive: Option<Duration>,
    ) -> Result<Self, ConnectError> {
        let start = Instant::now();
        let answer = loop {
            if let Some(line) = conn.read_line(Duration::from_millis(100))? {
                break line;
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(io::Error::from(io::ErrorKind::TimedOut).into());
            }
        };
        match answer.as_str() {
            WELCOME => {}
            BUSY => {
                conn.shutdown();
                return Err(ConnectError::Busy);
            }
            _ => return Err(io::Error::from(io::ErrorKind::InvalidData).into()),
        }
        let routes = Arc::new(Routes::default());
        routes.open.store(true, Ordering::SeqCst);
        routes.focus.store(1, Ordering::SeqCst);
        let (out, outbox) = mpsc::channel();
        let r = routes.clone();
        std::thread::Builder::new()
            .name("haml-client".into())
            .spawn(move || {
                pump(conn, &outbox, keepalive, &r);
                r.open.store(false, Ordering::SeqCst);
                let rigs = r.rigs.lock().unwrap_or_else(|e| e.into_inner()).clone();
                for rig in rigs {
                    fan(&rig, RigEvent::Link(Link::Disconnected));
                }
                fan(&r.tx, TxEvent::Error("the server is gone".into()));
            })?;
        Ok(Client { out, routes })
    }

    pub fn connected(&self) -> bool {
        self.routes.open.load(Ordering::SeqCst)
    }

    pub fn changed(&self) -> bool {
        self.routes.changed.swap(false, Ordering::SeqCst)
    }

    fn line(&self, text: &str) -> crate::Result<()> {
        if !self.connected() {
            return Err(Error::Disconnected);
        }
        self.out
            .send(Outgoing::Line(text.into()))
            .map_err(|_| Error::Disconnected)
    }

    pub fn list(&self, timeout: Duration) -> crate::Result<Listing> {
        let rx = listen(&self.routes.listing);
        self.line("LIST")?;
        let mut listing = Listing {
            rigs: Vec::new(),
            tx: Vec::new(),
            ports: Vec::new(),
            channels: Vec::new(),
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
            } else if let Some(rest) = line.strip_prefix("DEVICE app ") {
                let name = rest.split_once(' ').map_or("", |(_, n)| n);
                listing.channels.push(name.into());
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

    pub fn channel(&self, index: usize) -> RemoteChannel {
        RemoteChannel {
            client: self.clone(),
            index,
            events: listen(&self.routes.app(index)),
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
            pending: Vec::new(),
            phone: None,
        }
    }

    pub fn so2r(&self) -> RemoteSo2r {
        RemoteSo2r(self.clone())
    }

    pub fn ptt(&self, radio: u8) -> RemotePtt {
        RemotePtt {
            client: self.clone(),
            radio,
            held: None,
        }
    }

    pub fn close(&self) {
        let _ = self.out.send(Outgoing::Close);
    }
}

fn pump(
    mut conn: Box<dyn Conn>,
    outbox: &Receiver<Outgoing>,
    keepalive: Option<Duration>,
    r: &Routes,
) {
    let mut spoke = Instant::now();
    loop {
        let mut text = Vec::new();
        let mut close = false;
        loop {
            match outbox.try_recv() {
                Ok(Outgoing::Line(l)) => text.push(l),
                Ok(Outgoing::Close) | Err(mpsc::TryRecvError::Disconnected) => {
                    close = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if close {
            text.push("BYE".into());
        } else if text.is_empty() && keepalive.is_some_and(|k| spoke.elapsed() >= k) {
            text.push(PING.into());
        }
        if !text.is_empty() {
            spoke = Instant::now();
            if !say(conn.as_mut(), &text) {
                break;
            }
        }
        if close {
            break;
        }
        match conn.read_line(Duration::from_millis(5)) {
            Ok(Some(line)) => route(r, &line),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    conn.shutdown();
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
        "APP" => {
            let (i, l) = rest.split_once(' ').unwrap_or((rest, ""));
            if let Ok(i) = i.parse::<usize>() {
                fan(&r.app(i), l.to_string());
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
    pending: Vec<u64>,
    phone: Option<u64>,
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
        let id = crate::tx::ticket();
        self.pending.push(id);
        let _ = self.client.send(&TxCommand::Send {
            id,
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
        let c = match (on, self.phone) {
            (true, None) => {
                let id = crate::tx::ticket();
                self.phone = Some(id);
                TxCommand::Send {
                    id,
                    radio: self.radio(),
                    signal: Signal::Voice,
                }
            }
            (false, Some(id)) => {
                self.phone = None;
                TxCommand::Release(id)
            }
            _ => return,
        };
        let _ = self.client.send(&c);
    }

    fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    fn poll(&mut self) -> Vec<KeyerEvent> {
        let mut out = Vec::new();
        for e in self.events.try_iter() {
            match e {
                TxEvent::Finished { id, .. } if self.pending.contains(&id) => {
                    self.pending.retain(|p| *p != id);
                    if self.pending.is_empty() {
                        out.push(KeyerEvent::Finished);
                    }
                }
                TxEvent::Stopped => {
                    self.phone = None;
                    if !self.pending.is_empty() {
                        self.pending.clear();
                        out.push(KeyerEvent::Finished);
                    }
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
    held: Option<u64>,
}

impl Ptt for RemotePtt {
    fn set_ptt(&mut self, on: bool) -> crate::Result<()> {
        let (c, held) = match (on, self.held) {
            (true, None) => {
                let id = crate::tx::ticket();
                let c = TxCommand::Send {
                    id,
                    radio: self.radio,
                    signal: Signal::Voice,
                };
                (c, Some(id))
            }
            (false, Some(id)) => (TxCommand::Release(id), None),
            _ => return Ok(()),
        };
        self.client.send(&c)?;
        self.held = held;
        Ok(())
    }

    fn ptt(&self) -> bool {
        self.held.is_some()
    }
}
