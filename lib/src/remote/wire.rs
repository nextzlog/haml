use std::time::Duration;

use crate::keyer::KeyerEvent;
use crate::ports::Link;
use crate::so2r::Rx;
use crate::tx::{Signal, TxCommand, TxEvent};
use crate::{Command, Event, Mode, Outcome, RigCaps, RigEvent, Status, Vfo};

pub fn clean(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn vfo(v: Vfo) -> &'static str {
    match v {
        Vfo::A => "A",
        Vfo::B => "B",
    }
}

fn parse_vfo(s: &str) -> Option<Vfo> {
    match s {
        "A" => Some(Vfo::A),
        "B" => Some(Vfo::B),
        _ => None,
    }
}

fn parse_mode(s: &str) -> Option<Mode> {
    Mode::ALL.into_iter().find(|m| m.label() == s)
}

fn flag(on: bool) -> &'static str {
    if on {
        "1"
    } else {
        "0"
    }
}

fn parse_flag(s: &str) -> Option<bool> {
    match s {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn rx(r: Rx) -> &'static str {
    match r {
        Rx::Rig1 => "rig1",
        Rx::Rig2 => "rig2",
        Rx::Both => "both",
    }
}

fn parse_rx(s: &str) -> Option<Rx> {
    match s {
        "rig1" => Some(Rx::Rig1),
        "rig2" => Some(Rx::Rig2),
        "both" => Some(Rx::Both),
        _ => None,
    }
}

type Flag = fn(&mut RigCaps) -> &mut bool;

const CAPS: [(char, Flag); 8] = [
    ('f', |c| &mut c.freq),
    ('m', |c| &mut c.mode),
    ('v', |c| &mut c.vfo),
    ('s', |c| &mut c.split),
    ('p', |c| &mut c.ptt),
    ('r', |c| &mut c.rit),
    ('x', |c| &mut c.xit),
    ('t', |c| &mut c.transceive),
];

pub fn caps(c: &RigCaps) -> String {
    let mut c = *c;
    CAPS.iter()
        .map(|(k, f)| if *f(&mut c) { *k } else { '-' })
        .collect()
}

pub fn parse_caps(s: &str) -> Option<RigCaps> {
    if s.len() != CAPS.len() {
        return None;
    }
    let mut c = RigCaps::NONE;
    for ((k, f), got) in CAPS.iter().zip(s.chars()) {
        match got {
            g if g == *k => *f(&mut c) = true,
            '-' => {}
            _ => return None,
        }
    }
    Some(c)
}

fn words(s: &str) -> Vec<&str> {
    s.split(' ').collect()
}

pub fn command(c: &Command) -> String {
    match *c {
        Command::SetFreq(v, hz) => format!("freq {} {hz}", vfo(v)),
        Command::SetMode(v, m) => format!("mode {} {}", vfo(v), m.label()),
        Command::SelectVfo(v) => format!("vfo {}", vfo(v)),
        Command::Split(on) => format!("split {}", flag(on)),
        Command::Ptt(on) => format!("ptt {}", flag(on)),
        Command::Rit(on) => format!("rit {}", flag(on)),
        Command::Xit(on) => format!("xit {}", flag(on)),
        Command::RitOffset(hz) => format!("ritoffset {hz}"),
        Command::ClearRit => "clearrit".into(),
        Command::Transceive(on) => format!("transceive {}", flag(on)),
    }
}

pub fn parse_command(s: &str) -> Option<Command> {
    Some(match words(s).as_slice() {
        ["freq", v, hz] => Command::SetFreq(parse_vfo(v)?, hz.parse().ok()?),
        ["mode", v, m] => Command::SetMode(parse_vfo(v)?, parse_mode(m)?),
        ["vfo", v] => Command::SelectVfo(parse_vfo(v)?),
        ["split", b] => Command::Split(parse_flag(b)?),
        ["ptt", b] => Command::Ptt(parse_flag(b)?),
        ["rit", b] => Command::Rit(parse_flag(b)?),
        ["xit", b] => Command::Xit(parse_flag(b)?),
        ["ritoffset", hz] => Command::RitOffset(hz.parse().ok()?),
        ["clearrit"] => Command::ClearRit,
        ["transceive", b] => Command::Transceive(parse_flag(b)?),
        _ => return None,
    })
}

fn event(e: &Event) -> String {
    match *e {
        Event::Freq(v, hz) => format!("freq {} {hz}", vfo(v)),
        Event::Mode(m) => format!("mode {}", m.label()),
        Event::Vfo(v) => format!("vfo {}", vfo(v)),
        Event::Split(on) => format!("split {}", flag(on)),
        Event::Ptt(on) => format!("ptt {}", flag(on)),
        Event::Rit(on) => format!("rit {}", flag(on)),
        Event::Xit(on) => format!("xit {}", flag(on)),
        Event::RitOffset(hz) => format!("ritoffset {hz}"),
        Event::Ok => "ok".into(),
        Event::Rejected => "rejected".into(),
    }
}

fn parse_event(s: &str) -> Option<Event> {
    Some(match words(s).as_slice() {
        ["freq", v, hz] => Event::Freq(parse_vfo(v)?, hz.parse().ok()?),
        ["mode", m] => Event::Mode(parse_mode(m)?),
        ["vfo", v] => Event::Vfo(parse_vfo(v)?),
        ["split", b] => Event::Split(parse_flag(b)?),
        ["ptt", b] => Event::Ptt(parse_flag(b)?),
        ["rit", b] => Event::Rit(parse_flag(b)?),
        ["xit", b] => Event::Xit(parse_flag(b)?),
        ["ritoffset", hz] => Event::RitOffset(hz.parse().ok()?),
        ["ok"] => Event::Ok,
        ["rejected"] => Event::Rejected,
        _ => return None,
    })
}

const STATUSES: [(Status, &str); 5] = [
    (Status::Done, "done"),
    (Status::Sent, "sent"),
    (Status::Rejected, "rejected"),
    (Status::Failed, "failed"),
    (Status::Unsupported, "unsupported"),
];

pub fn rig_event(e: &RigEvent) -> String {
    match e {
        RigEvent::Event(e) => format!("event {}", event(e)),
        RigEvent::Outcome(o) => {
            let status = STATUSES
                .iter()
                .find(|s| s.0 == o.status)
                .map_or("", |s| s.1);
            format!(
                "outcome {status} {} {} {}",
                o.attempts,
                o.latency.as_micros(),
                command(&o.command)
            )
        }
        RigEvent::Link(Link::Connected) => "link up".into(),
        RigEvent::Link(Link::Disconnected) => "link down".into(),
    }
}

pub fn parse_rig_event(s: &str) -> Option<RigEvent> {
    let (head, rest) = s.split_once(' ')?;
    Some(match head {
        "event" => RigEvent::Event(parse_event(rest)?),
        "outcome" => {
            let mut w = rest.splitn(4, ' ');
            let status = w.next()?;
            let status = STATUSES.iter().find(|s| s.1 == status)?.0;
            let attempts = w.next()?.parse().ok()?;
            let latency = Duration::from_micros(w.next()?.parse().ok()?);
            let command = parse_command(w.next()?)?;
            RigEvent::Outcome(Outcome {
                command,
                status,
                attempts,
                latency,
            })
        }
        "link" => RigEvent::Link(match rest {
            "up" => Link::Connected,
            "down" => Link::Disconnected,
            _ => return None,
        }),
        _ => return None,
    })
}

fn signal(s: &Signal) -> String {
    match s {
        Signal::Cw(t) => format!("cw {}", clean(t)),
        Signal::Fsk(t) => format!("fsk {}", clean(t)),
        Signal::Voice => "voice".into(),
    }
}

fn parse_signal(s: &str) -> Option<Signal> {
    match s.split_once(' ') {
        Some(("cw", t)) => Some(Signal::Cw(t.into())),
        Some(("fsk", t)) => Some(Signal::Fsk(t.into())),
        None if s == "voice" => Some(Signal::Voice),
        _ => None,
    }
}

pub fn tx_command(c: &TxCommand) -> String {
    match c {
        TxCommand::Send { radio, signal: s } => format!("send {radio} {}", signal(s)),
        TxCommand::Release => "release".into(),
        TxCommand::Stop => "stop".into(),
        TxCommand::SetWpm(w) => format!("wpm {w}"),
        TxCommand::Focus(r) => format!("focus {r}"),
        TxCommand::Transmit(r) => format!("transmit {r}"),
        TxCommand::Listen(r) => format!("listen {}", rx(*r)),
        TxCommand::Blend(on, p) => format!("blend {} {p}", flag(*on)),
    }
}

pub fn parse_tx_command(s: &str) -> Option<TxCommand> {
    if let Some(rest) = s.strip_prefix("send ") {
        let (radio, sig) = rest.split_once(' ')?;
        return Some(TxCommand::Send {
            radio: radio.parse().ok()?,
            signal: parse_signal(sig)?,
        });
    }
    Some(match words(s).as_slice() {
        ["release"] => TxCommand::Release,
        ["stop"] => TxCommand::Stop,
        ["wpm", w] => TxCommand::SetWpm(w.parse().ok()?),
        ["focus", r] => TxCommand::Focus(r.parse().ok()?),
        ["transmit", r] => TxCommand::Transmit(r.parse().ok()?),
        ["listen", r] => TxCommand::Listen(parse_rx(r)?),
        ["blend", b, p] => TxCommand::Blend(parse_flag(b)?, p.parse().ok()?),
        _ => return None,
    })
}

pub fn tx_event(e: &TxEvent) -> String {
    match e {
        TxEvent::Ptt { radio, on } => format!("ptt {radio} {}", flag(*on)),
        TxEvent::Started { radio, signal: s } => format!("started {radio} {}", signal(s)),
        TxEvent::Afsk { radio, text } => format!("afsk {radio} {}", clean(text)),
        TxEvent::Finished { radio } => format!("finished {radio}"),
        TxEvent::Stopped => "stopped".into(),
        TxEvent::Focus(r) => format!("focus {r}"),
        TxEvent::Listen(r) => format!("listen {}", rx(*r)),
        TxEvent::Keyer(KeyerEvent::Finished) => "keyer finished".into(),
        TxEvent::Keyer(KeyerEvent::Speed(w)) => format!("keyer speed {w}"),
        TxEvent::Keyer(KeyerEvent::Paddle(on)) => format!("keyer paddle {}", flag(*on)),
        TxEvent::Keyer(KeyerEvent::Error(t)) => format!("keyer error {}", clean(t)),
        TxEvent::Error(t) => format!("error {}", clean(t)),
    }
}

pub fn parse_tx_event(s: &str) -> Option<TxEvent> {
    let (head, rest) = s.split_once(' ').unwrap_or((s, ""));
    let radio_and = |rest: &str| -> Option<(u8, String)> {
        let (r, t) = rest.split_once(' ').unwrap_or((rest, ""));
        Some((r.parse().ok()?, t.to_string()))
    };
    Some(match head {
        "ptt" => {
            let (radio, on) = radio_and(rest)?;
            TxEvent::Ptt {
                radio,
                on: parse_flag(&on)?,
            }
        }
        "started" => {
            let (radio, s) = radio_and(rest)?;
            TxEvent::Started {
                radio,
                signal: parse_signal(&s)?,
            }
        }
        "afsk" => {
            let (radio, text) = radio_and(rest)?;
            TxEvent::Afsk { radio, text }
        }
        "finished" => TxEvent::Finished {
            radio: rest.parse().ok()?,
        },
        "stopped" => TxEvent::Stopped,
        "focus" => TxEvent::Focus(rest.parse().ok()?),
        "listen" => TxEvent::Listen(parse_rx(rest)?),
        "keyer" => TxEvent::Keyer(match rest.split_once(' ').unwrap_or((rest, "")) {
            ("finished", _) => KeyerEvent::Finished,
            ("speed", w) => KeyerEvent::Speed(w.parse().ok()?),
            ("paddle", b) => KeyerEvent::Paddle(parse_flag(b)?),
            ("error", t) => KeyerEvent::Error(t.into()),
            _ => return None,
        }),
        "error" => TxEvent::Error(rest.into()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rig_commands_and_events_round_trip() {
        let commands = [
            Command::SetFreq(Vfo::B, 14_025_000),
            Command::SetMode(Vfo::A, Mode::CwR),
            Command::SelectVfo(Vfo::B),
            Command::Split(true),
            Command::Ptt(false),
            Command::Rit(true),
            Command::Xit(false),
            Command::RitOffset(-120),
            Command::ClearRit,
            Command::Transceive(true),
        ];
        for c in commands {
            assert_eq!(parse_command(&command(&c)), Some(c));
        }
        let events = [
            RigEvent::Event(Event::Freq(Vfo::A, 7_012_000)),
            RigEvent::Event(Event::Mode(Mode::RttyR)),
            RigEvent::Event(Event::Rejected),
            RigEvent::Link(Link::Disconnected),
            RigEvent::Outcome(Outcome {
                command: Command::SetFreq(Vfo::A, 1),
                status: Status::Failed,
                attempts: 3,
                latency: Duration::from_millis(12),
            }),
        ];
        for e in events {
            assert_eq!(parse_rig_event(&rig_event(&e)), Some(e));
        }
        assert_eq!(parse_command("freq C 1"), None);
    }

    #[test]
    fn rig_caps_round_trip() {
        let c = RigCaps {
            xit: false,
            transceive: false,
            ..RigCaps::ALL
        };
        assert_eq!(caps(&c), "fmvspr--");
        assert_eq!(parse_caps(&caps(&c)), Some(c));
        assert_eq!(parse_caps(&caps(&RigCaps::NONE)), Some(RigCaps::NONE));
        assert_eq!(parse_caps("fm"), None);
        assert_eq!(parse_caps("fmvsprxz"), None);
    }

    #[test]
    fn tx_commands_and_events_round_trip() {
        let commands = [
            TxCommand::Send {
                radio: 2,
                signal: Signal::Cw("CQ TEST".into()),
            },
            TxCommand::Send {
                radio: 1,
                signal: Signal::Voice,
            },
            TxCommand::Release,
            TxCommand::Stop,
            TxCommand::SetWpm(32),
            TxCommand::Focus(2),
            TxCommand::Transmit(1),
            TxCommand::Listen(Rx::Both),
            TxCommand::Blend(true, 40),
        ];
        for c in commands {
            assert_eq!(parse_tx_command(&tx_command(&c)), Some(c));
        }
        let events = [
            TxEvent::Ptt { radio: 1, on: true },
            TxEvent::Started {
                radio: 2,
                signal: Signal::Fsk("RY RY".into()),
            },
            TxEvent::Afsk {
                radio: 1,
                text: "CQ".into(),
            },
            TxEvent::Finished { radio: 2 },
            TxEvent::Stopped,
            TxEvent::Focus(1),
            TxEvent::Listen(Rx::Rig2),
            TxEvent::Keyer(KeyerEvent::Speed(30)),
            TxEvent::Keyer(KeyerEvent::Error("gone".into())),
            TxEvent::Error("no radio 3".into()),
        ];
        for e in events {
            assert_eq!(parse_tx_event(&tx_event(&e)), Some(e));
        }
        assert_eq!(
            tx_command(&TxCommand::Send {
                radio: 1,
                signal: Signal::Cw("A\nB".into())
            }),
            "send 1 cw A B"
        );
    }
}
