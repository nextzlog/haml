use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::keyer::winkeyer::*;
use crate::morse::{keying, total, Timing};
use crate::transport::{Emulated, Emulator, Transport};

pub const REVISION: u8 = 23;
pub const ADMIN_ECHO: u8 = 0x04;
pub const STATUS: u8 = 0x15;
pub const MIN_SIM_WPM: u8 = 5;
pub const POINTER: u8 = 0x16;
pub const CANCEL_BUFFERED_SPEED: u8 = 0x1e;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub open: bool,
    pub wpm: u8,
    pub mode: u8,
    pub pincfg: u8,
    pub sidetone: u8,
    pub ptt: bool,
    pub ptt_delay: (u8, u8),
    pub pot_min: u8,
    pub pot_range: u8,
    pub pot: u8,
    pub paddle: bool,
    pub sent: String,
    pub buffer: String,
    pub neo: Vec<u8>,
}

impl Default for State {
    fn default() -> Self {
        State {
            open: false,
            wpm: 0,
            mode: 0,
            pincfg: 0,
            sidetone: 0,
            ptt: false,
            ptt_delay: (0, 0),
            pot_min: POT_MIN_WPM,
            pot_range: 50,
            pot: 20,
            paddle: false,
            sent: String::new(),
            buffer: String::new(),
            neo: Vec::new(),
        }
    }
}

impl State {
    pub fn busy(&self) -> bool {
        !self.buffer.is_empty()
    }

    fn status(&self) -> u8 {
        let mut s = 0xc0;
        if self.busy() {
            s |= STATUS_BUSY;
        }
        if self.paddle {
            s |= STATUS_BREAKIN;
        }
        s
    }
}

struct Inner {
    state: State,
    input: Vec<u8>,
    out: Vec<u8>,
    queue: VecDeque<(Duration, String)>,
    speed: Option<u8>,
    start: Instant,
    manual: Option<Duration>,
}

#[derive(Clone)]
pub struct WinKeyerSim(Emulated<Inner>);

impl Default for WinKeyerSim {
    fn default() -> Self {
        WinKeyerSim::new()
    }
}

fn arguments(command: u8, sub: Option<u8>) -> usize {
    match command {
        ADMIN => match sub {
            None => 1,
            Some(ADMIN_CAL | ADMIN_ECHO | 14 | 15 | 22 | 25) => 2,
            Some(19) => 3,
            Some(13) => 257,
            Some(_) => 1,
        },
        POINTER => match sub {
            None | Some(0) => 1,
            Some(_) => 2,
        },
        0x07 | 0x08 | 0x0a | 0x13 | 0x15 | 0x1e | 0x1f => 0,
        0x04 | MERGE => 2,
        SET_SPEEDPOT => 3,
        0x0f => 15,
        _ => 1,
    }
}

impl Inner {
    fn now(&self) -> Duration {
        self.manual.unwrap_or_else(|| self.start.elapsed())
    }

    fn timing(&self) -> Timing {
        Timing {
            wpm: u32::from(self.speed.unwrap_or(self.state.wpm).max(MIN_SIM_WPM)),
            ..Timing::default()
        }
    }

    fn key(&mut self, text: String) {
        let was = self.state.status();
        let len = total(&keying(&text.replace('|', "_"), &self.timing()));
        let from = self.queue.back().map_or(self.now(), |(t, _)| *t);
        self.queue
            .push_back((from.max(self.now()) + len, text.clone()));
        self.state.buffer.push_str(&text);
        if self.state.status() != was {
            let s = self.state.status();
            self.out.push(s);
        }
    }

    fn clear(&mut self) {
        let was = self.state.status();
        self.queue.clear();
        self.state.buffer.clear();
        self.speed = None;
        if self.state.status() != was {
            let s = self.state.status();
            self.out.push(s);
        }
    }

    fn advance(&mut self) {
        let now = self.now();
        while self.queue.front().is_some_and(|(t, _)| *t <= now) {
            let (_, text) = self.queue.pop_front().unwrap_or_default();
            let n = self.state.buffer.len().min(text.len());
            self.state.buffer.drain(..n);
            self.state.sent.push_str(&text);
            if self.state.mode & MODE_SERIAL_ECHOBACK != 0 {
                self.out.extend(text.bytes());
            }
            if !self.state.busy() {
                self.speed = None;
                let s = self.state.status();
                self.out.push(s);
            }
        }
    }

    fn command(&mut self, c: &[u8]) {
        match c {
            [ADMIN, ADMIN_OPEN] => {
                self.state.open = true;
                self.out.push(REVISION);
            }
            [ADMIN, ADMIN_CLOSE] => {
                self.clear();
                self.state.open = false;
                self.state.ptt = false;
            }
            [ADMIN, ADMIN_ECHO, b] => self.out.push(*b),
            [ADMIN, ..] => {}
            _ if !self.state.open => {}
            [SIDETONE, v] => self.state.sidetone = *v,
            [SET_WPM, v] => self.state.wpm = *v,
            [SET_PTT_DELAY, a, b] => self.state.ptt_delay = (*a, *b),
            [SET_SPEEDPOT, min, range, _] => {
                self.state.pot_min = *min;
                self.state.pot_range = *range;
            }
            [GET_SPEEDPOT] => self.out.push(0x80 | self.state.pot.min(0x3f)),
            [SET_PINCFG, v] => self.state.pincfg = *v,
            [CLEAR] => self.clear(),
            [SET_MODE, v] => self.state.mode = *v,
            [STATUS] => {
                let s = self.state.status();
                self.out.push(s);
            }
            [PTT, v] => self.state.ptt = *v != 0,
            [MERGE, a, b] => self.key(format!("{}{}", *a as char, *b as char)),
            [BUFFERED_SPEED, v] => self.speed = Some(*v),
            [CANCEL_BUFFERED_SPEED] => self.speed = None,
            [b] if *b >= 0x20 && *b < 0x80 => self.key((*b as char).to_string()),
            _ => {}
        }
    }

    fn take_commands(&mut self) {
        let mut i = 0;
        while i < self.input.len() {
            let b = self.input[i];
            if b >= 0x80 {
                if b != 0xff {
                    self.state.neo.push(b);
                }
                i += 1;
                continue;
            }
            let n = if b < 0x20 {
                arguments(b, self.input.get(i + 1).copied())
            } else {
                0
            };
            if i + n >= self.input.len() {
                break;
            }
            let c = self.input[i..=i + n].to_vec();
            self.command(&c);
            i += n + 1;
        }
        self.input.drain(..i);
    }
}

impl WinKeyerSim {
    pub fn new() -> Self {
        WinKeyerSim(Emulated::new(Inner {
            state: State::default(),
            input: Vec::new(),
            out: Vec::new(),
            queue: VecDeque::new(),
            speed: None,
            start: Instant::now(),
            manual: None,
        }))
    }

    pub fn manual() -> Self {
        let s = WinKeyerSim::new();
        s.lock().manual = Some(Duration::ZERO);
        s
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0 .0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn transport(&self) -> impl Transport + Send + use<> {
        self.0.clone()
    }

    pub fn state(&self) -> State {
        let mut g = self.lock();
        g.advance();
        g.state.clone()
    }

    pub fn wait(&self, d: Duration) {
        let mut g = self.lock();
        if let Some(t) = g.manual.as_mut() {
            *t += d;
        }
        g.advance();
    }

    pub fn press_paddle(&self, down: bool) {
        let mut g = self.lock();
        if g.state.paddle == down {
            return;
        }
        if down {
            g.queue.clear();
            g.state.buffer.clear();
            g.speed = None;
        }
        g.state.paddle = down;
        let s = g.state.status();
        g.out.push(s);
    }

    pub fn turn_pot(&self, wpm: u8) {
        let mut g = self.lock();
        let p = wpm
            .saturating_sub(g.state.pot_min)
            .min(g.state.pot_range)
            .min(0x3f);
        if p != g.state.pot {
            g.state.pot = p;
            if g.state.open {
                g.out.push(0x80 | p);
            }
        }
    }

    pub fn pot_wpm(&self) -> u8 {
        let g = self.lock();
        g.state.pot_min + g.state.pot
    }
}

impl Emulator for Inner {
    fn receive(&mut self, bytes: &[u8]) {
        self.advance();
        self.input.extend_from_slice(bytes);
        self.take_commands();
    }

    fn send(&mut self, out: &mut Vec<u8>) {
        self.advance();
        out.append(&mut self.out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyer::{Keyer, KeyerEvent};
    use crate::transport::Boxed;

    fn opened(setup: &Setup) -> (WinKeyerSim, WinKeyer<Boxed>) {
        let sim = WinKeyerSim::manual();
        let mut k = WinKeyer::open(Box::new(sim.transport()) as Boxed, setup).unwrap();
        assert!(sim.state().open);
        assert_eq!(k.poll(), [KeyerEvent::Speed(25)], "the speed pot");
        (sim, k)
    }

    fn setup() -> Setup {
        Setup {
            wpm: 30,
            paddle_swap: true,
            sidetone: false,
            ptt_before_ms: 50,
            ptt_after_ms: 120,
            speed_pot: true,
        }
    }

    #[test]
    fn the_driver_opens_and_sets_up_the_emulated_keyer() {
        let (sim, k) = opened(&setup());
        let s = sim.state();
        assert_eq!(s.wpm, 25);
        assert_eq!(s.mode, MODE_SERIAL_ECHOBACK | MODE_PADDLE_SWAP);
        assert_eq!(s.pincfg, 0xad);
        assert_eq!(s.sidetone, 0x86);
        assert_eq!(s.ptt_delay, (5, 12));
        assert_eq!((s.pot_min, s.pot_range), (5, 50));
        assert!(s.neo.is_empty() && !s.ptt);
        drop(k);
        assert!(!sim.state().open);
    }

    #[test]
    fn a_message_is_keyed_echoed_and_finished() {
        let (sim, mut k) = opened(&setup());
        assert!(k.poll().is_empty());
        k.send("CQ a");
        assert!(sim.state().busy());
        assert_eq!(sim.state().buffer, "CQ AR");
        assert!(k.poll().is_empty());
        assert!(k.busy());
        sim.wait(Duration::from_millis(800));
        let s = sim.state();
        assert_eq!((s.sent.as_str(), s.buffer.as_str()), ("C", "Q AR"));
        sim.wait(Duration::from_secs(5));
        assert_eq!(k.poll(), [KeyerEvent::Finished]);
        assert!(!k.busy());
        assert_eq!(sim.state().sent, "CQ AR");
    }

    #[test]
    fn abort_clears_the_buffer() {
        let (sim, mut k) = opened(&setup());
        k.send("TEST TEST");
        k.poll();
        k.abort();
        assert!(!sim.state().busy());
        sim.wait(Duration::from_secs(5));
        assert!(sim.state().sent.is_empty());
        assert!(k.poll().is_empty());
    }

    #[test]
    fn buffered_speed_lasts_for_the_message() {
        let (sim, mut k) = opened(&setup());
        k.send("\\+9EEEEEEEEEE");
        sim.wait(Duration::from_secs(1));
        let fast = sim.state().sent.len();
        sim.wait(Duration::from_secs(5));
        k.poll();
        k.send("EEEEEEEEEE");
        sim.wait(Duration::from_secs(1));
        let normal = sim.state().sent.len() - 10;
        assert!(fast > normal, "{fast} {normal}");
    }

    #[test]
    fn paddle_and_speed_pot_reach_the_driver() {
        let (sim, mut k) = opened(&setup());
        k.send("CQ CQ CQ");
        k.poll();
        sim.press_paddle(true);
        assert_eq!(k.poll(), [KeyerEvent::Paddle(true), KeyerEvent::Finished]);
        assert!(!sim.state().busy());
        sim.press_paddle(false);
        assert_eq!(k.poll(), [KeyerEvent::Paddle(false)]);
        sim.turn_pot(35);
        assert_eq!(k.poll(), [KeyerEvent::Speed(35)]);
        assert_eq!((sim.state().wpm, sim.pot_wpm()), (35, 35));
        sim.turn_pot(200);
        assert_eq!(k.poll(), [KeyerEvent::Speed(55)]);
    }

    #[test]
    fn ptt_and_neo_bytes_are_kept() {
        let (sim, mut k) = opened(&setup());
        k.set_ptt(true);
        assert!(sim.state().ptt);
        let mut p = sim.transport();
        p.write(&[0x95, 0x87, 0xc7, 0xcf]).unwrap();
        k.set_ptt(false);
        let s = sim.state();
        assert!(!s.ptt);
        assert_eq!(s.neo, [0x95, 0x87, 0xc7, 0xcf]);
    }

    #[test]
    fn commands_split_across_writes_and_status_and_echo() {
        let sim = WinKeyerSim::manual();
        let mut p = sim.transport();
        p.write(&[SET_WPM, 20]).unwrap();
        assert_eq!(sim.state().wpm, 0, "closed");
        p.write(&[ADMIN]).unwrap();
        p.write(&[ADMIN_OPEN, SET_PTT_DELAY, 1]).unwrap();
        p.write(&[2, ADMIN, ADMIN_ECHO]).unwrap();
        p.write(&[0x55, STATUS]).unwrap();
        let mut got = Vec::new();
        p.read(&mut got).unwrap();
        assert_eq!(got, [REVISION, 0x55, 0xc0]);
        assert_eq!(sim.state().ptt_delay, (1, 2));
    }

    #[test]
    fn calibrate_pointer_and_cancel_speed_take_their_arguments() {
        let sim = WinKeyerSim::manual();
        let mut p = sim.transport();
        p.write(&[ADMIN, ADMIN_CAL, 0xff, ADMIN, ADMIN_OPEN])
            .unwrap();
        p.write(&[POINTER, 0, POINTER, 1, 0x41, POINTER, 3, 2])
            .unwrap();
        p.write(&[SET_WPM, 20, BUFFERED_SPEED, 40, CANCEL_BUFFERED_SPEED])
            .unwrap();
        p.write(b"E").unwrap();
        let s = sim.state();
        assert!(s.open && s.neo.is_empty());
        assert_eq!((s.wpm, s.buffer.as_str()), (20, "E"));
        let mut got = Vec::new();
        p.read(&mut got).unwrap();
        assert_eq!(got[0], REVISION);
        let g = sim.lock();
        assert_eq!(g.speed, None);
        assert_eq!(g.timing().wpm, 20);
    }

    #[test]
    fn eeprom_load_and_rtty_registers_take_their_arguments() {
        let sim = WinKeyerSim::manual();
        let mut p = sim.transport();
        p.write(&[ADMIN, ADMIN_OPEN, ADMIN, 19, 0x41, 0x42])
            .unwrap();
        let mut eeprom = vec![ADMIN, 13];
        eeprom.extend((0..=255).map(|_| 0x45));
        p.write(&eeprom).unwrap();
        p.write(&[SET_WPM, 20]).unwrap();
        p.write(b"E").unwrap();
        let s = sim.state();
        assert_eq!((s.wpm, s.buffer.as_str()), (20, "E"));
    }
}
