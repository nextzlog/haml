use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::hid::Hid;
use crate::keyer::if4cw::report::{self, KEY, MIC, PTT, RIG};
use crate::morse::Decoder;

pub const PRODUCT: &str = "USBIF4CW Ver.2.3 (simulator)";
const IDLE: u8 = 0x07;
const DIT: u8 = 0x01;
const DAH: u8 = 0x04;
const KEPT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub attached: usize,
    pub bits: u8,
    pub paddle_wpm: Option<u32>,
    pub paddle_reverse: bool,
    pub ptt_delays: Option<(u8, u8)>,
    pub dit: bool,
    pub dah: bool,
    pub keyed: String,
}

impl State {
    pub fn key(&self) -> bool {
        self.bits & KEY == 0
    }

    pub fn ptt(&self) -> bool {
        self.bits & PTT == 0
    }

    pub fn rig(&self) -> u8 {
        if self.bits & RIG == 0 {
            2
        } else {
            1
        }
    }

    pub fn mic(&self) -> u8 {
        if self.bits & MIC == 0 {
            2
        } else {
            1
        }
    }
}

struct Inner {
    state: State,
    input: VecDeque<[u8; 8]>,
    decoder: Decoder,
    start: Instant,
    manual: Option<Duration>,
}

impl Inner {
    fn now(&self) -> Duration {
        self.manual.unwrap_or_else(|| self.start.elapsed())
    }

    fn settle(&mut self) {
        let now = self.now();
        self.decoder.settle(now);
        self.state.keyed = self.decoder.text().to_string();
    }

    fn key(&mut self, down: bool) {
        let now = self.now();
        self.decoder.key(down, now);
        self.state.keyed = self.decoder.text().to_string();
    }

    fn answer(&self) -> [u8; 8] {
        let (dit, dah) = if self.state.paddle_reverse {
            (DAH, DIT)
        } else {
            (DIT, DAH)
        };
        let mut b = IDLE;
        if self.state.dit {
            b &= !dit;
        }
        if self.state.dah {
            b &= !dah;
        }
        [0x04, b, 0x04, 0, 0, 0, 0, 0]
    }

    fn report(&mut self, r: &[u8; 9]) {
        match r[1] {
            0x01 => {
                let was = self.state.key();
                self.state.bits = r[2];
                if self.state.key() != was {
                    self.key(!was);
                }
            }
            0x04 if *r == report::QUERY => {
                let a = self.answer();
                self.input.push_back(a);
            }
            0x22 => self.state.ptt_delays = Some((r[2], r[3])),
            0x24 if r[2] > 0 => {
                let wpm = (600.0 / f64::from(r[2])).round() as u32;
                self.state.paddle_wpm = Some(wpm);
                self.decoder
                    .hint(Duration::from_millis(1200 / u64::from(wpm.max(1))));
            }
            0x25 if r[2] == 0 => self.state.ptt_delays = None,
            0x26 => self.state.paddle_reverse = r[4] != 0,
            _ => {}
        }
    }
}

#[derive(Clone)]
pub struct If4cwSim(Arc<Mutex<Inner>>);

impl Default for If4cwSim {
    fn default() -> Self {
        If4cwSim::new()
    }
}

impl If4cwSim {
    pub fn new() -> Self {
        If4cwSim(Arc::new(Mutex::new(Inner {
            state: State {
                attached: 0,
                bits: 0xFF,
                paddle_wpm: None,
                paddle_reverse: false,
                ptt_delays: None,
                dit: false,
                dah: false,
                keyed: String::new(),
            },
            input: VecDeque::new(),
            decoder: Decoder::new(KEPT),
            start: Instant::now(),
            manual: None,
        })))
    }

    pub fn manual() -> Self {
        let s = If4cwSim::new();
        s.lock().manual = Some(Duration::ZERO);
        s
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn hid(&self) -> SimHid {
        self.lock().state.attached += 1;
        SimHid(self.clone())
    }

    pub fn state(&self) -> State {
        let mut g = self.lock();
        g.settle();
        g.state.clone()
    }

    pub fn wait(&self, d: Duration) {
        let mut g = self.lock();
        if let Some(t) = g.manual.as_mut() {
            *t += d;
        }
        g.settle();
    }

    pub fn press(&self, dit: bool, dah: bool) {
        let mut g = self.lock();
        g.state.dit = dit;
        g.state.dah = dah;
    }

    pub fn clear(&self) {
        let mut g = self.lock();
        g.decoder.clear();
        g.state.keyed.clear();
    }
}

pub struct SimHid(If4cwSim);

impl Drop for SimHid {
    fn drop(&mut self) {
        let mut g = self.0.lock();
        g.state.attached = g.state.attached.saturating_sub(1);
    }
}

impl Hid for SimHid {
    fn write(&mut self, report: &[u8]) -> io::Result<()> {
        if let Ok(r) = report.try_into() {
            self.0.lock().report(r);
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut g = self.0.lock();
        g.settle();
        match g.input.pop_front() {
            Some(r) => {
                let n = r.len().min(buf.len());
                buf[..n].copy_from_slice(&r[..n]);
                Ok(n)
            }
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn keyed_marks_read_back_as_letters() {
        let sim = If4cwSim::manual();
        let mut h = sim.hid();
        h.write(&report::wpm(30)).unwrap();
        for (down, up) in [(40, 40), (120, 160), (40, 280)] {
            h.write(&report::port(0xFE)).unwrap();
            sim.wait(ms(down));
            h.write(&report::port(0xFF)).unwrap();
            sim.wait(ms(up));
        }
        assert_eq!(sim.state().keyed, "AE ");
        assert_eq!(sim.state().paddle_wpm, Some(30));
        drop(h);
        assert_eq!(sim.state().attached, 0);
    }

    #[test]
    fn an_unknown_pattern_is_a_star() {
        let sim = If4cwSim::manual();
        let mut h = sim.hid();
        h.write(&report::wpm(30)).unwrap();
        for _ in 0..9 {
            h.write(&report::port(0xFE)).unwrap();
            sim.wait(ms(40));
            h.write(&report::port(0xFF)).unwrap();
            sim.wait(ms(40));
        }
        sim.wait(ms(400));
        assert_eq!(sim.state().keyed, "* ");
    }

    #[test]
    fn the_query_is_answered_with_the_paddle() {
        let sim = If4cwSim::manual();
        let mut h = sim.hid();
        let mut buf = [0u8; 16];
        assert_eq!(h.read(&mut buf).unwrap(), 0);
        h.write(&report::QUERY).unwrap();
        sim.press(true, false);
        h.write(&report::QUERY).unwrap();
        h.write(&report::paddle(true)).unwrap();
        h.write(&report::QUERY).unwrap();
        let mut got = Vec::new();
        while h.read(&mut buf).unwrap() > 0 {
            got.push(buf[1]);
        }
        assert_eq!(got, [0x07, 0x06, 0x03]);
        h.write(&report::ptt_delays(30, 10)[0]).unwrap();
        assert_eq!(sim.state().ptt_delays, Some((30, 50)));
        h.write(&report::ptt_delays(0, 0)[0]).unwrap();
        assert_eq!(sim.state().ptt_delays, None);
    }

    #[test]
    fn port_bits_read_as_lines() {
        let sim = If4cwSim::manual();
        let mut h = sim.hid();
        let s = sim.state();
        assert!(!s.key() && !s.ptt() && s.rig() == 1 && s.mic() == 1);
        h.write(&report::port(0xF0)).unwrap();
        let s = sim.state();
        assert!(s.key() && s.ptt() && s.rig() == 2 && s.mic() == 2);
    }
}
