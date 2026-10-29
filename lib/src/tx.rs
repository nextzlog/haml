use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::device::{Device, Driver};
use crate::keyer::{Keyer, KeyerEvent};
use crate::ptt::Ptt;
use crate::so2r::{Rx, So2r};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Cw(String),
    Fsk(String),
    Voice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxCommand {
    Send { radio: u8, signal: Signal },
    Release,
    Stop,
    SetWpm(u32),
    Focus(u8),
    Transmit(u8),
    Listen(Rx),
    Blend(bool, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxEvent {
    Ptt { radio: u8, on: bool },
    Started { radio: u8, signal: Signal },
    Afsk { radio: u8, text: String },
    Finished { radio: u8 },
    Stopped,
    Focus(u8),
    Listen(Rx),
    Keyer(KeyerEvent),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delays {
    pub before: Duration,
    pub after: Duration,
}

impl Default for Delays {
    fn default() -> Self {
        Delays {
            before: Duration::from_millis(25),
            after: Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Idle,
    Lead {
        radio: u8,
        until: Instant,
        signal: Signal,
    },
    Sending {
        radio: u8,
        signal: Signal,
        held: bool,
    },
    Tail {
        radio: u8,
        until: Instant,
    },
}

pub struct Arbiter {
    keyer: Option<Box<dyn Keyer>>,
    so2r: Option<Box<dyn So2r>>,
    ptt: Vec<Option<Box<dyn Ptt>>>,
    delays: Delays,
    queue: VecDeque<(u8, Signal)>,
    phase: Phase,
    keyed_ptt: Option<u8>,
    phone: Option<u8>,
    on_air: Option<u8>,
}

impl Arbiter {
    pub fn new(radios: usize, delays: Delays) -> Self {
        Arbiter {
            keyer: None,
            so2r: None,
            ptt: (0..radios).map(|_| None).collect(),
            delays,
            queue: VecDeque::new(),
            phase: Phase::Idle,
            keyed_ptt: None,
            phone: None,
            on_air: None,
        }
    }

    pub fn set_keyer(&mut self, keyer: Option<Box<dyn Keyer>>, out: &mut Vec<TxEvent>) {
        if self.busy() {
            self.stop(out);
        }
        self.keyer = keyer;
    }

    pub fn set_so2r(&mut self, so2r: Option<Box<dyn So2r>>) {
        self.on_air = None;
        self.so2r = so2r;
    }

    pub fn set_ptt(&mut self, radio: u8, ptt: Option<Box<dyn Ptt>>) {
        if let Some(slot) = self.ptt.get_mut((radio as usize).wrapping_sub(1)) {
            *slot = ptt;
        }
    }

    pub fn set_delays(&mut self, delays: Delays) {
        self.delays = delays;
    }

    pub fn set_timing(&mut self, timing: crate::Timing) {
        if let Some(k) = self.keyer.as_mut() {
            k.set_timing(timing);
        }
    }

    pub fn keyer(&self) -> Option<&dyn Keyer> {
        self.keyer.as_deref()
    }

    pub fn so2r(&self) -> Option<&dyn So2r> {
        self.so2r.as_deref()
    }

    pub fn busy(&self) -> bool {
        self.transmitting().is_some()
            || !self.queue.is_empty()
            || self.keyer.as_ref().is_some_and(|k| k.busy())
    }

    pub fn on_air(&self) -> Option<u8> {
        self.on_air
    }

    pub fn transmit_on(&mut self, radio: u8) {
        if self.on_air != Some(radio) {
            self.on_air = Some(radio);
            if let Some(b) = self.so2r.as_mut() {
                b.tx(radio);
            }
        }
    }

    pub fn with_keyer(mut self, keyer: Box<dyn Keyer>) -> Self {
        self.keyer = Some(keyer);
        self
    }

    pub fn with_so2r(mut self, so2r: Box<dyn So2r>) -> Self {
        self.set_so2r(Some(so2r));
        self
    }

    pub fn with_ptt(mut self, radio: u8, ptt: Box<dyn Ptt>) -> Self {
        if let Some(slot) = self.ptt.get_mut(radio as usize - 1) {
            *slot = Some(ptt);
        }
        self
    }

    pub fn spawn(self) -> Device<TxCommand, TxEvent> {
        Device::spawn("haml-tx", self)
    }

    pub fn transmitting(&self) -> Option<u8> {
        match self.phase {
            Phase::Idle => None,
            Phase::Lead { radio, .. }
            | Phase::Sending { radio, .. }
            | Phase::Tail { radio, .. } => Some(radio),
        }
    }

    fn keyer_keys_ptt(&self, signal: &Signal) -> bool {
        matches!(signal, Signal::Cw(_))
            && self
                .keyer
                .as_ref()
                .is_some_and(|k| k.caps().ptt && k.caps().ptt_delays)
    }

    fn ptt(&mut self, radio: u8, on: bool, voice: bool, out: &mut Vec<TxEvent>) {
        let Some(Some(p)) = self.ptt.get_mut(radio as usize - 1) else {
            if let Some(k) = self.keyer.as_mut().filter(|_| voice || !on) {
                if (self.phone == Some(radio)) != on {
                    k.set_phone_ptt(on);
                    self.phone = on.then_some(radio);
                    out.push(TxEvent::Ptt { radio, on });
                }
            }
            return;
        };
        if p.ptt() == on {
            return;
        }
        match p.set_ptt(on) {
            Ok(()) => out.push(TxEvent::Ptt { radio, on }),
            Err(e) => out.push(TxEvent::Error(e.to_string())),
        }
    }

    fn begin(&mut self, radio: u8, signal: Signal, now: Instant, out: &mut Vec<TxEvent>) {
        self.transmit_on(radio);
        if self.keyer_keys_ptt(&signal) {
            self.start(radio, signal, out);
        } else {
            let voice = signal == Signal::Voice;
            self.ptt(radio, true, voice, out);
            self.phase = Phase::Lead {
                radio,
                until: now + self.delays.before,
                signal,
            };
        }
    }

    fn start(&mut self, radio: u8, signal: Signal, out: &mut Vec<TxEvent>) {
        let mut held = false;
        match &signal {
            Signal::Cw(text) => match self.keyer.as_mut() {
                Some(k) => {
                    k.send(text);
                    if k.caps().ptt && k.caps().ptt_delays {
                        self.keyed_ptt = Some(radio);
                    }
                }
                None => out.push(TxEvent::Error("no keyer for CW".into())),
            },
            Signal::Fsk(text) => {
                let device = self.keyer.as_mut().filter(|k| k.caps().fsk);
                match device.map(|k| k.send_fsk(text)) {
                    Some(Ok(())) => {}
                    Some(Err(e)) => out.push(TxEvent::Error(e.to_string())),
                    None => {
                        held = true;
                        out.push(TxEvent::Afsk {
                            radio,
                            text: text.clone(),
                        });
                    }
                }
            }
            Signal::Voice => held = true,
        }
        out.push(TxEvent::Started {
            radio,
            signal: signal.clone(),
        });
        self.phase = Phase::Sending {
            radio,
            signal,
            held,
        };
    }

    fn finish(&mut self, radio: u8, now: Instant, out: &mut Vec<TxEvent>) {
        out.push(TxEvent::Finished { radio });
        self.keyed_ptt = None;
        if let Some(i) = self.queue.iter().position(|(r, _)| *r == radio) {
            if i == 0 {
                let (_, next) = self.queue.pop_front().expect("found");
                if self.keyer_keys_ptt(&next) || self.ptt_on(radio) {
                    self.start(radio, next, out);
                    return;
                }
                self.queue.push_front((radio, next));
            }
        }
        if self.ptt_on(radio) {
            self.phase = Phase::Tail {
                radio,
                until: now + self.delays.after,
            };
        } else {
            self.phase = Phase::Idle;
        }
    }

    fn ptt_on(&self, radio: u8) -> bool {
        self.phone == Some(radio)
            || matches!(self.ptt.get(radio as usize - 1), Some(Some(p)) if p.ptt())
    }

    pub fn stop(&mut self, out: &mut Vec<TxEvent>) {
        self.queue.clear();
        if let Some(k) = self.keyer.as_mut() {
            k.abort();
            if self.keyed_ptt.take().is_some() {
                k.set_ptt(false);
            }
        }
        for radio in 1..=self.ptt.len() as u8 {
            self.ptt(radio, false, false, out);
        }
        self.phase = Phase::Idle;
        out.push(TxEvent::Stopped);
    }
}

impl Driver for Arbiter {
    type Command = TxCommand;
    type Event = TxEvent;

    fn handle(&mut self, command: TxCommand, now: Instant, out: &mut Vec<TxEvent>) {
        match command {
            TxCommand::Send { radio, signal } => {
                if radio == 0 || radio as usize > self.ptt.len() {
                    out.push(TxEvent::Error(format!("no radio {radio}")));
                } else {
                    self.queue.push_back((radio, signal));
                }
            }
            TxCommand::Release => {
                if let Phase::Sending {
                    radio, held: true, ..
                } = self.phase
                {
                    self.finish(radio, now, out);
                }
            }
            TxCommand::Stop => self.stop(out),
            TxCommand::SetWpm(wpm) => {
                if let Some(k) = self.keyer.as_mut() {
                    k.set_wpm(wpm);
                }
            }
            TxCommand::Focus(radio) => {
                let busy = self.transmitting().is_some();
                if !busy {
                    self.on_air = Some(radio);
                }
                if let Some(b) = self.so2r.as_mut() {
                    if !busy {
                        b.select_rig(radio);
                    } else {
                        b.rx(Rx::of(radio));
                    }
                }
                out.push(TxEvent::Focus(radio));
            }
            TxCommand::Transmit(radio) => {
                if self.transmitting().is_none() {
                    self.transmit_on(radio);
                }
            }
            TxCommand::Listen(rx) => {
                if let Some(b) = self.so2r.as_mut() {
                    b.rx(rx);
                }
                out.push(TxEvent::Listen(rx));
            }
            TxCommand::Blend(on, percent) => {
                if let Some(b) = self.so2r.as_mut() {
                    b.blend(on, percent);
                }
            }
        }
        for _ in 0..4 {
            let before = self.phase.clone();
            self.tick(now, out);
            if self.phase == before {
                break;
            }
        }
    }

    fn tick(&mut self, now: Instant, out: &mut Vec<TxEvent>) {
        let mut finished = false;
        if let Some(k) = self.keyer.as_mut() {
            for e in k.poll() {
                finished |= e == KeyerEvent::Finished;
                out.push(TxEvent::Keyer(e));
            }
        }
        if let Some(b) = self.so2r.as_mut() {
            b.poll();
        }
        match self.phase.clone() {
            Phase::Idle => {
                if let Some((radio, signal)) = self.queue.pop_front() {
                    self.begin(radio, signal, now, out);
                }
            }
            Phase::Lead {
                radio,
                until,
                signal,
            } if now >= until => self.start(radio, signal, out),
            Phase::Sending {
                radio, held: false, ..
            } => {
                let idle = self.keyer.as_ref().is_none_or(|k| !k.busy());
                if finished || idle {
                    self.finish(radio, now, out);
                }
            }
            Phase::Tail { radio, until } => {
                if self.queue.front().is_some_and(|(r, _)| *r == radio) {
                    let (_, next) = self.queue.pop_front().expect("front");
                    self.start(radio, next, out);
                } else if now >= until {
                    self.ptt(radio, false, false, out);
                    self.phase = Phase::Idle;
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, out: &mut Vec<TxEvent>) {
        self.stop(out);
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(2)
    }
}

impl Drop for Arbiter {
    fn drop(&mut self) {
        if self.transmitting().is_some() {
            self.stop(&mut Vec::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyer::KeyerCaps;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    struct FakePtt(Log, u8, bool);

    impl Ptt for FakePtt {
        fn set_ptt(&mut self, on: bool) -> crate::Result<()> {
            self.2 = on;
            self.0.lock().unwrap().push(format!("ptt{} {on}", self.1));
            Ok(())
        }
        fn ptt(&self) -> bool {
            self.2
        }
    }

    struct FakeKeyer {
        log: Log,
        busy: Arc<Mutex<bool>>,
        hardware_ptt: bool,
    }

    impl Keyer for FakeKeyer {
        fn caps(&self) -> KeyerCaps {
            KeyerCaps {
                ptt: self.hardware_ptt,
                ptt_delays: self.hardware_ptt,
                ..KeyerCaps::default()
            }
        }
        fn send(&mut self, text: &str) {
            *self.busy.lock().unwrap() = true;
            self.log.lock().unwrap().push(format!("cw {text}"));
        }
        fn abort(&mut self) {
            *self.busy.lock().unwrap() = false;
            self.log.lock().unwrap().push("abort".into());
        }
        fn set_wpm(&mut self, _wpm: u32) {}
        fn wpm(&self) -> u32 {
            25
        }
        fn set_ptt(&mut self, on: bool) {
            self.log.lock().unwrap().push(format!("keyer ptt {on}"));
        }
        fn busy(&self) -> bool {
            *self.busy.lock().unwrap()
        }
        fn poll(&mut self) -> Vec<KeyerEvent> {
            Vec::new()
        }
    }

    struct Box2(Log);

    impl So2r for Box2 {
        fn caps(&self) -> crate::So2rCaps {
            crate::So2rCaps::default()
        }
        fn tx(&mut self, radio: u8) {
            self.0.lock().unwrap().push(format!("tx{radio}"));
        }
        fn rx(&mut self, rx: Rx) {
            self.0.lock().unwrap().push(format!("{rx:?}"));
        }
    }

    fn station(hardware_ptt: bool) -> (Arbiter, Log, Arc<Mutex<bool>>) {
        let log = Log::default();
        let busy = Arc::new(Mutex::new(false));
        let delays = Delays {
            before: Duration::from_millis(20),
            after: Duration::from_millis(10),
        };
        let a = Arbiter::new(2, delays)
            .with_keyer(Box::new(FakeKeyer {
                log: log.clone(),
                busy: busy.clone(),
                hardware_ptt,
            }))
            .with_so2r(Box::new(Box2(log.clone())))
            .with_ptt(1, Box::new(FakePtt(log.clone(), 1, false)))
            .with_ptt(2, Box::new(FakePtt(log.clone(), 2, false)));
        (a, log, busy)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn take(log: &Log) -> Vec<String> {
        std::mem::take(&mut log.lock().unwrap())
    }

    #[test]
    fn ptt_leads_and_trails_the_keying() {
        let (mut a, log, busy) = station(false);
        let t = Instant::now();
        let mut out = Vec::new();
        a.handle(
            TxCommand::Send {
                radio: 2,
                signal: Signal::Cw("CQ".into()),
            },
            t,
            &mut out,
        );
        assert_eq!(take(&log), ["tx2", "ptt2 true"]);
        a.tick(t + ms(10), &mut out);
        assert!(take(&log).is_empty(), "still in the lead");
        a.tick(t + ms(20), &mut out);
        assert_eq!(take(&log), ["cw CQ"]);
        *busy.lock().unwrap() = false;
        a.tick(t + ms(30), &mut out);
        a.tick(t + ms(35), &mut out);
        assert!(take(&log).is_empty(), "still in the tail");
        a.tick(t + ms(40), &mut out);
        assert_eq!(take(&log), ["ptt2 false"]);
        assert!(out.contains(&TxEvent::Finished { radio: 2 }));
        assert_eq!(a.transmitting(), None);
    }

    #[test]
    fn a_second_radio_waits_until_the_first_is_off() {
        let (mut a, log, busy) = station(false);
        let t = Instant::now();
        let mut out = Vec::new();
        let cw = |radio, s: &str| TxCommand::Send {
            radio,
            signal: Signal::Cw(s.into()),
        };
        a.handle(cw(1, "CQ"), t, &mut out);
        a.handle(cw(2, "TU"), t, &mut out);
        a.tick(t + ms(20), &mut out);
        assert_eq!(take(&log), ["tx1", "ptt1 true", "cw CQ"]);
        a.tick(t + ms(25), &mut out);
        assert!(take(&log).is_empty(), "radio 2 waits while radio 1 sends");
        *busy.lock().unwrap() = false;
        a.tick(t + ms(30), &mut out);
        a.tick(t + ms(40), &mut out);
        a.tick(t + ms(41), &mut out);
        assert_eq!(take(&log), ["ptt1 false", "tx2", "ptt2 true"]);
    }

    #[test]
    fn a_hardware_keyer_keys_its_own_ptt() {
        let (mut a, log, _busy) = station(true);
        let t = Instant::now();
        let mut out = Vec::new();
        a.handle(
            TxCommand::Send {
                radio: 1,
                signal: Signal::Cw("E".into()),
            },
            t,
            &mut out,
        );
        assert_eq!(take(&log), ["tx1", "cw E"]);
    }

    #[test]
    fn stop_releases_everything() {
        let (mut a, log, _busy) = station(true);
        let t = Instant::now();
        let mut out = Vec::new();
        a.handle(
            TxCommand::Send {
                radio: 1,
                signal: Signal::Cw("CQ CQ".into()),
            },
            t,
            &mut out,
        );
        a.handle(
            TxCommand::Send {
                radio: 2,
                signal: Signal::Voice,
            },
            t,
            &mut out,
        );
        take(&log);
        a.handle(TxCommand::Stop, t, &mut out);
        assert_eq!(take(&log), ["abort", "keyer ptt false"]);
        assert_eq!(out.last(), Some(&TxEvent::Stopped));
        a.tick(t + ms(100), &mut out);
        assert!(take(&log).is_empty(), "the queue is gone");
    }

    #[test]
    fn fsk_without_a_device_asks_for_afsk_and_voice_waits_for_release() {
        let (mut a, log, _busy) = station(false);
        let t = Instant::now();
        let mut out = Vec::new();
        a.handle(
            TxCommand::Send {
                radio: 1,
                signal: Signal::Fsk("RYRY".into()),
            },
            t,
            &mut out,
        );
        a.tick(t + ms(20), &mut out);
        assert!(out.contains(&TxEvent::Afsk {
            radio: 1,
            text: "RYRY".into()
        }));
        a.tick(t + ms(500), &mut out);
        assert_eq!(a.transmitting(), Some(1), "held until released");
        a.handle(TxCommand::Release, t + ms(500), &mut out);
        a.tick(t + ms(510), &mut out);
        assert_eq!(take(&log), ["tx1", "ptt1 true", "ptt1 false"]);
    }

    #[test]
    fn focus_moves_only_the_receiver_while_transmitting() {
        let (mut a, log, _busy) = station(true);
        let t = Instant::now();
        let mut out = Vec::new();
        a.handle(TxCommand::Focus(2), t, &mut out);
        assert_eq!(take(&log), ["tx2", "Rig2"]);
        a.handle(
            TxCommand::Send {
                radio: 2,
                signal: Signal::Cw("CQ".into()),
            },
            t,
            &mut out,
        );
        take(&log);
        a.handle(TxCommand::Focus(1), t, &mut out);
        assert_eq!(take(&log), ["Rig1"]);
    }

    #[test]
    fn without_a_ptt_line_voice_keys_the_keyer_and_the_box_is_told_once() {
        let log = Log::default();
        let busy = Arc::new(Mutex::new(false));
        let mut a = Arbiter::new(
            2,
            Delays {
                before: Duration::ZERO,
                after: Duration::ZERO,
            },
        );
        a.set_keyer(
            Some(Box::new(FakeKeyer {
                log: log.clone(),
                busy,
                hardware_ptt: true,
            })),
            &mut Vec::new(),
        );
        a.set_so2r(Some(Box::new(Box2(log.clone()))));
        let t = Instant::now();
        let mut out = Vec::new();
        let voice = TxCommand::Send {
            radio: 2,
            signal: Signal::Voice,
        };
        a.handle(voice.clone(), t, &mut out);
        assert!(a.busy());
        a.handle(TxCommand::Release, t, &mut out);
        a.handle(voice, t, &mut out);
        a.handle(TxCommand::Release, t, &mut out);
        assert_eq!(
            take(&log),
            [
                "tx2",
                "keyer ptt true",
                "keyer ptt false",
                "keyer ptt true",
                "keyer ptt false"
            ]
        );
        assert!(!a.busy());
        a.transmit_on(1);
        a.transmit_on(1);
        assert_eq!(take(&log), ["tx1"]);
        assert!(a.keyer().is_some() && a.so2r().is_some());
    }

    #[test]
    fn the_device_thread_stops_tx_when_dropped() {
        let (a, log, _busy) = station(false);
        let d = a.spawn();
        let events = d.subscribe();
        d.send(TxCommand::Send {
            radio: 1,
            signal: Signal::Voice,
        })
        .unwrap();
        let start = Instant::now();
        while !events
            .try_iter()
            .any(|e| matches!(e, TxEvent::Started { .. }))
        {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(ms(2));
        }
        drop(d);
        assert_eq!(log.lock().unwrap().last().unwrap(), "ptt1 false");
    }
}
