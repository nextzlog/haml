pub mod ascii;
pub mod cat;
pub mod icom;
pub mod model;
pub mod protocol;
pub mod yaesu;

use std::time::{Duration, Instant};

use crate::device::{Commander, Device, Driver};
use crate::ports::Link;
use crate::ptt::Ptt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Lsb,
    Usb,
    Cw,
    CwR,
    Am,
    Fm,
    Rtty,
    RttyR,
    Data,
    Dv,
    Other,
}

impl Mode {
    pub const ALL: [Mode; 11] = [
        Mode::Lsb,
        Mode::Usb,
        Mode::Cw,
        Mode::CwR,
        Mode::Am,
        Mode::Fm,
        Mode::Rtty,
        Mode::RttyR,
        Mode::Data,
        Mode::Dv,
        Mode::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Lsb => "LSB",
            Mode::Usb => "USB",
            Mode::Cw => "CW",
            Mode::CwR => "CW-R",
            Mode::Am => "AM",
            Mode::Fm => "FM",
            Mode::Rtty => "RTTY",
            Mode::RttyR => "RTTY-R",
            Mode::Data => "DATA",
            Mode::Dv => "DV",
            Mode::Other => "OTHER",
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Vfo {
    #[default]
    A,
    B,
}

impl Vfo {
    pub fn other(self) -> Vfo {
        match self {
            Vfo::A => Vfo::B,
            Vfo::B => Vfo::A,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SetFreq(Vfo, u64),
    SetMode(Vfo, Mode),
    SelectVfo(Vfo),
    Split(bool),
    Ptt(bool),
    Rit(bool),
    Xit(bool),
    RitOffset(i32),
    ClearRit,
    Transceive(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Freq(Vfo, u64),
    Mode(Mode),
    Vfo(Vfo),
    Split(bool),
    Ptt(bool),
    Rit(bool),
    Xit(bool),
    RitOffset(i32),
    Ok,
    Rejected,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    pub freq_a: Option<u64>,
    pub freq_b: Option<u64>,
    pub vfo: Vfo,
    pub mode: Option<Mode>,
    pub split: Option<bool>,
    pub ptt: Option<bool>,
    pub rit: Option<bool>,
    pub xit: Option<bool>,
    pub rit_offset: Option<i32>,
}

impl State {
    pub fn freq(&self) -> Option<u64> {
        self.freq_of(self.vfo)
    }

    pub fn freq_of(&self, vfo: Vfo) -> Option<u64> {
        match vfo {
            Vfo::A => self.freq_a,
            Vfo::B => self.freq_b,
        }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Freq(Vfo::A, hz) => self.freq_a = Some(hz),
            Event::Freq(Vfo::B, hz) => self.freq_b = Some(hz),
            Event::Mode(m) => self.mode = Some(m),
            Event::Vfo(v) => self.vfo = v,
            Event::Split(on) => self.split = Some(on),
            Event::Ptt(on) => self.ptt = Some(on),
            Event::Rit(on) => self.rit = Some(on),
            Event::Xit(on) => self.xit = Some(on),
            Event::RitOffset(hz) => self.rit_offset = Some(hz),
            Event::Ok | Event::Rejected => {}
        }
    }

    pub fn confirms(&self, command: &Command) -> bool {
        match *command {
            Command::SetFreq(v, hz) => self.freq_of(v) == Some(hz),
            Command::SetMode(_, Mode::Cw) => matches!(self.mode, Some(Mode::Cw | Mode::CwR)),
            Command::SetMode(_, m) => self.mode == Some(m),
            Command::SelectVfo(v) => self.vfo == v,
            Command::Split(on) => self.split == Some(on),
            Command::Ptt(on) => self.ptt == Some(on),
            Command::Rit(on) => self.rit == Some(on),
            Command::Xit(on) => self.xit == Some(on),
            _ => true,
        }
    }
}

pub fn answers(command: &Command, event: &Event) -> bool {
    matches!(
        (command, event),
        (Command::SetFreq(a, _), Event::Freq(b, _)) if a == b
    ) || matches!(
        (command, event),
        (Command::SetMode(..), Event::Mode(_))
            | (Command::SelectVfo(_), Event::Vfo(_))
            | (Command::Split(_), Event::Split(_))
            | (Command::Ptt(_), Event::Ptt(_))
            | (Command::Rit(_), Event::Rit(_))
            | (Command::Xit(_), Event::Xit(_))
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Done,
    Sent,
    Rejected,
    Failed,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub command: Command,
    pub status: Status,
    pub attempts: u32,
    pub latency: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RigCaps {
    pub freq: bool,
    pub mode: bool,
    pub vfo: bool,
    pub split: bool,
    pub ptt: bool,
    pub rit: bool,
    pub xit: bool,
    pub transceive: bool,
}

impl RigCaps {
    pub const ALL: RigCaps = RigCaps {
        freq: true,
        mode: true,
        vfo: true,
        split: true,
        ptt: true,
        rit: true,
        xit: true,
        transceive: true,
    };

    pub const NONE: RigCaps = RigCaps {
        freq: false,
        mode: false,
        vfo: false,
        split: false,
        ptt: false,
        rit: false,
        xit: false,
        transceive: false,
    };

    pub fn supports(&self, command: &Command) -> bool {
        match command {
            Command::SetFreq(..) => self.freq,
            Command::SetMode(..) => self.mode,
            Command::SelectVfo(_) => self.vfo,
            Command::Split(_) => self.split,
            Command::Ptt(_) => self.ptt,
            Command::Rit(_) | Command::RitOffset(_) | Command::ClearRit => self.rit,
            Command::Xit(_) => self.xit,
            Command::Transceive(_) => self.transceive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigEvent {
    Event(Event),
    Outcome(Outcome),
    Link(Link),
}

pub trait Rig: Send {
    fn caps(&self) -> RigCaps;

    fn send(&mut self, command: Command, now: Instant);

    fn tick(&mut self, now: Instant) -> Vec<Event>;

    fn take_outcomes(&mut self) -> Vec<Outcome>;

    fn state(&self) -> State;

    fn link(&self) -> Link {
        Link::Connected
    }

    fn busy(&self) -> bool {
        false
    }

    fn error(&self) -> Option<String> {
        None
    }

    fn timeouts(&self) -> u64 {
        0
    }
}

impl<R: Rig + ?Sized> Rig for Box<R> {
    fn caps(&self) -> RigCaps {
        (**self).caps()
    }

    fn send(&mut self, command: Command, now: Instant) {
        (**self).send(command, now)
    }

    fn tick(&mut self, now: Instant) -> Vec<Event> {
        (**self).tick(now)
    }

    fn take_outcomes(&mut self) -> Vec<Outcome> {
        (**self).take_outcomes()
    }

    fn state(&self) -> State {
        (**self).state()
    }

    fn link(&self) -> Link {
        (**self).link()
    }

    fn busy(&self) -> bool {
        (**self).busy()
    }

    fn error(&self) -> Option<String> {
        (**self).error()
    }

    fn timeouts(&self) -> u64 {
        (**self).timeouts()
    }
}

pub struct RigDriver<R> {
    rig: R,
    link: Option<Link>,
}

impl<R: Rig + 'static> RigDriver<R> {
    pub fn new(rig: R) -> Self {
        RigDriver { rig, link: None }
    }

    pub fn spawn(rig: R) -> Device<Command, RigEvent> {
        Device::spawn("haml-rig", RigDriver::new(rig))
    }
}

impl<R: Rig + 'static> Driver for RigDriver<R> {
    type Command = Command;
    type Event = RigEvent;

    fn handle(&mut self, command: Command, now: Instant, _out: &mut Vec<RigEvent>) {
        self.rig.send(command, now);
    }

    fn tick(&mut self, now: Instant, out: &mut Vec<RigEvent>) {
        let before = self.rig.state();
        let events = self.rig.tick(now);
        let after = self.rig.state();
        if before != after {
            out.extend(events.into_iter().map(RigEvent::Event));
        }
        out.extend(self.rig.take_outcomes().into_iter().map(RigEvent::Outcome));
        let link = self.rig.link();
        if self.link != Some(link) {
            self.link = Some(link);
            out.push(RigEvent::Link(link));
        }
    }
}

pub struct RigClient {
    commands: Commander<Command>,
    events: std::sync::mpsc::Receiver<RigEvent>,
    caps: RigCaps,
    state: State,
    outcomes: Vec<Outcome>,
    link: Link,
}

impl RigClient {
    pub fn new(device: &Device<Command, RigEvent>, caps: RigCaps) -> Self {
        RigClient {
            commands: device.commander(),
            events: device.subscribe(),
            caps,
            state: State::default(),
            outcomes: Vec::new(),
            link: Link::Connected,
        }
    }
}

impl Rig for RigClient {
    fn caps(&self) -> RigCaps {
        self.caps
    }

    fn send(&mut self, command: Command, _now: Instant) {
        if self.commands.send(command).is_err() {
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

pub struct RigPtt {
    rig: Commander<Command>,
    on: bool,
}

impl RigPtt {
    pub fn new(rig: Commander<Command>) -> Self {
        RigPtt { rig, on: false }
    }
}

impl Ptt for RigPtt {
    fn set_ptt(&mut self, on: bool) -> crate::Result<()> {
        self.rig.send(Command::Ptt(on))?;
        self.on = on;
        Ok(())
    }

    fn ptt(&self) -> bool {
        self.on
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_gate_commands() {
        let caps = RigCaps {
            xit: false,
            ..RigCaps::ALL
        };
        assert!(caps.supports(&Command::Rit(true)));
        assert!(!caps.supports(&Command::Xit(true)));
    }

    #[test]
    fn state_confirms_what_it_shows() {
        let mut s = State::default();
        s.apply(Event::Freq(Vfo::A, 7_000_000));
        s.apply(Event::Mode(Mode::CwR));
        assert!(s.confirms(&Command::SetFreq(Vfo::A, 7_000_000)));
        assert!(s.confirms(&Command::SetMode(Vfo::A, Mode::Cw)));
        assert!(!s.confirms(&Command::Split(true)));
        assert!(answers(&Command::Split(true), &Event::Split(false)));
    }
}
