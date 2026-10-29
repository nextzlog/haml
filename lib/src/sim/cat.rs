use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::rig::ascii::Dialect;
use crate::rig::model::Family;
use crate::rig::{icom, yaesu};
use crate::{Mode, Transport, Vfo};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impairments {
    pub latency: Duration,
    pub jitter: Duration,
    pub drop: f64,
    pub garble: f64,
    pub chunk: usize,
    pub echo: bool,
    pub collision: f64,
    pub transceive: Option<Duration>,
}

impl Default for Impairments {
    fn default() -> Self {
        Impairments {
            latency: Duration::from_millis(5),
            jitter: Duration::ZERO,
            drop: 0.0,
            garble: 0.0,
            chunk: 0,
            echo: false,
            collision: 0.0,
            transceive: None,
        }
    }
}

impl Impairments {
    pub fn clean() -> Self {
        Self::default()
    }

    pub fn harsh() -> Self {
        Impairments {
            latency: Duration::from_millis(20),
            jitter: Duration::from_millis(30),
            drop: 0.005,
            garble: 0.005,
            chunk: 3,
            echo: true,
            collision: 0.05,
            transceive: Some(Duration::from_millis(250)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Radio {
    pub freq_a: u64,
    pub freq_b: u64,
    pub vfo: Vfo,
    pub mode: Mode,
    pub split: bool,
    pub ptt: bool,
    pub rit: bool,
    pub xit: bool,
    pub rit_offset: i32,
    pub auto_info: bool,
    pub power: bool,
}

impl Default for Radio {
    fn default() -> Self {
        Radio {
            freq_a: 7_012_000,
            freq_b: 7_100_000,
            vfo: Vfo::A,
            mode: Mode::Cw,
            split: false,
            ptt: false,
            rit: false,
            xit: false,
            rit_offset: 0,
            auto_info: false,
            power: true,
        }
    }
}

impl Radio {
    pub fn freq(&self) -> u64 {
        self.freq_of(self.vfo)
    }

    pub fn freq_of(&self, vfo: Vfo) -> u64 {
        match vfo {
            Vfo::A => self.freq_a,
            Vfo::B => self.freq_b,
        }
    }

    fn set_freq(&mut self, vfo: Vfo, hz: u64) {
        match vfo {
            Vfo::A => self.freq_a = hz,
            Vfo::B => self.freq_b = hz,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SimStats {
    pub commands: u64,
    pub collisions: u64,
    pub dropped: u64,
    pub garbled: u64,
    pub unsolicited: u64,
}

#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn chance(&mut self, p: f64) -> bool {
        p > 0.0 && self.unit() < p
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

struct Inner {
    family: Family,
    radio: Radio,
    imp: Impairments,
    rng: Rng,
    now: Instant,
    realtime: bool,
    inbuf: Vec<u8>,
    out: VecDeque<(Instant, u8)>,
    last_ready: Instant,
    next_unsolicited: Option<Instant>,
    stats: SimStats,
    received: VecDeque<Vec<u8>>,
}

pub const RECEIVED_KEPT: usize = 100;

#[derive(Clone)]
pub struct Simulator(Arc<Mutex<Inner>>);

#[derive(Clone)]
pub struct SimTransport(Arc<Mutex<Inner>>);

impl Simulator {
    pub fn new(family: Family, impairments: Impairments, seed: u64) -> Self {
        let now = Instant::now();
        Simulator(Arc::new(Mutex::new(Inner {
            family,
            radio: Radio::default(),
            imp: impairments,
            rng: Rng::new(seed),
            now,
            realtime: false,
            inbuf: Vec::new(),
            out: VecDeque::new(),
            last_ready: now,
            next_unsolicited: impairments.transceive.map(|d| now + d),
            stats: SimStats::default(),
            received: VecDeque::new(),
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn transport(&self) -> SimTransport {
        SimTransport(self.0.clone())
    }

    pub fn advance(&self, now: Instant) {
        let mut g = self.lock();
        g.now = g.now.max(now);
        let t = g.now;
        g.pump(t);
    }

    pub fn realtime(&self, on: bool) {
        self.lock().realtime = on;
    }

    pub fn set_impairments(&self, impairments: Impairments) {
        let mut g = self.lock();
        g.imp = impairments;
        g.next_unsolicited = impairments.transceive.map(|d| g.now + d);
    }

    pub fn radio(&self) -> Radio {
        self.lock().radio
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Radio) -> R) -> R {
        f(&mut self.lock().radio)
    }

    pub fn stats(&self) -> SimStats {
        self.lock().stats
    }

    pub fn received(&self) -> Vec<Vec<u8>> {
        self.lock().received.iter().cloned().collect()
    }

    pub fn turn_dial(&self, hz: u64) {
        let mut g = self.lock();
        let vfo = g.radio.vfo;
        g.radio.set_freq(vfo, hz);
        if g.radio.auto_info {
            g.unsolicited();
        }
    }

    pub fn press_mode(&self, mode: Mode) {
        let mut g = self.lock();
        g.radio.mode = mode;
        if g.radio.auto_info {
            let now = g.now;
            let msg = g.mode_message(true);
            g.emit(&msg, now);
        }
    }
}

impl SimTransport {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Transport for SimTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut g = self.lock();
        let now = g.clock();
        g.write(bytes, now);
        Ok(())
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut g = self.lock();
        let now = g.clock();
        g.pump(now);
        let limit = match g.imp.chunk {
            0 => usize::MAX,
            n => 1 + g.rng.below(n as u64) as usize,
        };
        let mut n = 0;
        while n < limit {
            match g.out.front() {
                Some((t, _)) if *t <= now => {
                    buf.push(g.out.pop_front().unwrap().1);
                    n += 1;
                }
                _ => break,
            }
        }
        Ok(n)
    }
}

impl Inner {
    fn clock(&mut self) -> Instant {
        if self.realtime {
            self.now = self.now.max(Instant::now());
        }
        self.now
    }

    fn pump(&mut self, now: Instant) {
        let Some(period) = self.imp.transceive.filter(|p| !p.is_zero()) else {
            return;
        };
        while let Some(t) = self.next_unsolicited.filter(|t| *t <= now) {
            self.next_unsolicited = Some(t + period);
            if self.radio.power {
                self.unsolicited_at(t);
            }
        }
    }

    fn unsolicited(&mut self) {
        let now = self.now;
        self.unsolicited_at(now);
    }

    fn unsolicited_at(&mut self, t: Instant) {
        let msg = self.freq_message(self.radio.vfo, true);
        if !msg.is_empty() {
            self.stats.unsolicited += 1;
            self.emit(&msg, t);
        }
    }

    fn impair(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            if self.rng.chance(self.imp.drop) {
                self.stats.dropped += 1;
            } else if self.rng.chance(self.imp.garble) {
                self.stats.garbled += 1;
                out.push(b ^ (1 << self.rng.below(8)));
            } else {
                out.push(b);
            }
        }
        out
    }

    fn emit(&mut self, bytes: &[u8], at: Instant) {
        let bytes = self.impair(bytes);
        let at = at.max(self.last_ready);
        self.last_ready = at;
        self.out.extend(bytes.into_iter().map(|b| (at, b)));
    }

    fn write(&mut self, bytes: &[u8], now: Instant) {
        let icom = matches!(self.family, Family::Icom { .. });
        if icom && self.rng.chance(self.imp.collision) {
            self.stats.collisions += 1;
            let at = now.max(self.last_ready);
            self.last_ready = at;
            self.out.extend([icom::JAM; 3].map(|b| (at, b)));
            return;
        }
        if icom && self.imp.echo {
            let at = now.max(self.last_ready);
            self.last_ready = at;
            self.out.extend(bytes.iter().map(|b| (at, *b)));
        }
        let bytes = self.impair(bytes);
        self.inbuf.extend(bytes);
        for cmd in self.split_commands() {
            self.stats.commands += 1;
            self.received.push_back(cmd.clone());
            if self.received.len() > RECEIVED_KEPT {
                self.received.pop_front();
            }
            if self.radio.power {
                let reply = self.answer(&cmd);
                if !reply.is_empty() {
                    let delay = self.imp.latency + self.imp.jitter.mul_f64(self.rng.unit());
                    self.emit(&reply, now + delay);
                }
            }
        }
        if self.family == Family::YaesuBinary {
            self.inbuf.clear();
        }
    }

    fn split_commands(&mut self) -> Vec<Vec<u8>> {
        let mut cmds = Vec::new();
        match self.family {
            Family::YaesuBinary => {
                while self.inbuf.len() >= 5 {
                    cmds.push(self.inbuf.drain(..5).collect());
                }
            }
            Family::Icom { .. } => {
                while let Some(end) = self.inbuf.iter().position(|b| *b == icom::END) {
                    let frame: Vec<u8> = self.inbuf.drain(..=end).collect();
                    if let Some(start) = frame.windows(2).rposition(|w| w == [icom::PREAMBLE; 2]) {
                        cmds.push(frame[start..].to_vec());
                    }
                }
            }
            Family::Ascii(_) => {
                while let Some(end) = self.inbuf.iter().position(|b| *b == b';') {
                    cmds.push(self.inbuf.drain(..=end).collect());
                }
            }
        }
        if self.inbuf.len() > 256 {
            self.inbuf.clear();
        }
        cmds
    }

    fn answer(&mut self, cmd: &[u8]) -> Vec<u8> {
        match self.family {
            Family::Icom { address } => self.answer_icom(address, cmd),
            Family::Ascii(d) => self.answer_ascii(d, cmd),
            Family::YaesuBinary => self.answer_binary(cmd),
        }
    }

    fn freq_message(&self, vfo: Vfo, unasked: bool) -> Vec<u8> {
        let hz = self.radio.freq_of(vfo);
        match self.family {
            Family::Icom { address } => {
                let (to, cmd) = if unasked {
                    (icom::BROADCAST, 0x00)
                } else {
                    (icom::CONTROLLER, 0x03)
                };
                let mut body = vec![cmd];
                body.extend(icom::to_bcd(hz, icom::freq_bytes(hz)));
                icom::frame(to, address, &body)
            }
            Family::Ascii(d) => {
                let cmd = if vfo == Vfo::A { "FA" } else { "FB" };
                format!("{cmd}{hz:0w$};", w = d.digits()).into_bytes()
            }
            Family::YaesuBinary => Vec::new(),
        }
    }

    fn mode_message(&self, unasked: bool) -> Vec<u8> {
        match self.family {
            Family::Icom { address } => {
                let (to, cmd) = if unasked {
                    (icom::BROADCAST, 0x01)
                } else {
                    (icom::CONTROLLER, 0x04)
                };
                let code = icom::mode_code(self.radio.mode).unwrap_or(0x12);
                icom::frame(to, address, &[cmd, code, 0x01])
            }
            Family::Ascii(d) => {
                let code = ascii_code(d, self.radio.mode);
                if d.is_yaesu() {
                    format!("MD0{code};").into_bytes()
                } else {
                    format!("MD{code};").into_bytes()
                }
            }
            Family::YaesuBinary => Vec::new(),
        }
    }

    fn answer_icom(&mut self, address: u8, cmd: &[u8]) -> Vec<u8> {
        if cmd.len() < 6 || cmd[2] != address {
            return Vec::new();
        }
        let r = &mut self.radio;
        let ok = icom::frame(icom::CONTROLLER, address, &[icom::OK]);
        let ng = icom::frame(icom::CONTROLLER, address, &[icom::NG]);
        let reply = |body: &[u8]| icom::frame(icom::CONTROLLER, address, body);
        let body = &cmd[4..cmd.len() - 1];
        match body {
            [0x03] => self.freq_message(self.radio.vfo, false),
            [0x04] => self.mode_message(false),
            [0x05, bcd @ ..] if matches!(bcd.len(), 5 | 6) => match icom::from_bcd(bcd) {
                Some(hz) => {
                    r.set_freq(r.vfo, hz);
                    ok
                }
                None => ng,
            },
            [0x06, code, ..] => match icom::mode_code(icom::mode_of(*code)) {
                Some(_) => {
                    r.mode = icom::mode_of(*code);
                    ok
                }
                None => ng,
            },
            [0x07, v @ (0x00 | 0x01)] => {
                r.vfo = if *v == 0 { Vfo::A } else { Vfo::B };
                ok
            }
            [0x0F] => reply(&[0x0F, r.split as u8]),
            [0x0F, s @ (0 | 1)] => {
                r.split = *s == 1;
                ok
            }
            [0x1C, 0x00] => reply(&[0x1C, 0x00, r.ptt as u8]),
            [0x1C, 0x00, p @ (0 | 1)] => {
                r.ptt = *p == 1;
                ok
            }
            [0x21, 0x00] => {
                let mut b = vec![0x21, 0x00];
                b.extend(icom::to_bcd(r.rit_offset.unsigned_abs() as u64, 2));
                b.push((r.rit_offset < 0) as u8);
                reply(&b)
            }
            [0x21, 0x00, lo, hi, sign @ (0 | 1)] => match icom::from_bcd(&[*lo, *hi]) {
                Some(hz) => {
                    r.rit_offset = if *sign == 1 { -(hz as i32) } else { hz as i32 };
                    ok
                }
                None => ng,
            },
            [0x21, 0x01] => reply(&[0x21, 0x01, r.rit as u8]),
            [0x21, 0x02] => reply(&[0x21, 0x02, r.xit as u8]),
            [0x21, 0x01, on @ (0 | 1)] => {
                r.rit = *on == 1;
                ok
            }
            [0x21, 0x02, on @ (0 | 1)] => {
                r.xit = *on == 1;
                ok
            }
            _ => ng,
        }
    }

    fn info(&self, d: Dialect, vfo: Vfo) -> Vec<u8> {
        let r = &self.radio;
        let hz = r.freq_of(vfo);
        let code = ascii_code(d, r.mode);
        let w = d.digits();
        let off = format!(
            "{}{:04}",
            if r.rit_offset < 0 { '-' } else { '+' },
            r.rit_offset.unsigned_abs()
        );
        let (rit, xit) = (r.rit as u8, r.xit as u8);
        if d.is_yaesu() {
            let cmd = if vfo == Vfo::A { "IF" } else { "OI" };
            format!("{cmd}001{hz:0w$}{off}{rit}{xit}{code}00000;").into_bytes()
        } else {
            let v = if r.vfo == Vfo::A { '0' } else { '1' };
            let (tx, split) = (r.ptt as u8, r.split as u8);
            format!("IF{hz:0w$}     {off}{rit}{xit}000{tx}{code}{v}0{split}0000;").into_bytes()
        }
    }

    fn answer_ascii(&mut self, d: Dialect, cmd: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(cmd);
        let body = text.trim_end_matches(';');
        let yaesu = d.is_yaesu();
        let w = d.digits();
        let nak = b"?;".to_vec();
        let r = &mut self.radio;
        let bit = |s: &str| match s {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        };
        let vfo_of = |s: &str| match s {
            "0" => Some(Vfo::A),
            "1" => Some(Vfo::B),
            _ => None,
        };
        if !body.is_ascii() {
            return nak;
        }
        let (head, rest) = body.split_at(body.len().min(2));
        match (head, rest) {
            ("FA", "") => self.freq_message(Vfo::A, false),
            ("FB", "") => self.freq_message(Vfo::B, false),
            ("FA" | "FB", digits) if digits.len() == w => match digits.parse() {
                Ok(hz) if digits.bytes().all(|b| b.is_ascii_digit()) => {
                    r.set_freq(if head == "FA" { Vfo::A } else { Vfo::B }, hz);
                    Vec::new()
                }
                _ => nak,
            },
            ("MD", "") if !yaesu => self.mode_message(false),
            ("MD", "0" | "1") if yaesu => self.mode_message(false),
            ("MD", p) => {
                let (vfo_ok, code) = if yaesu {
                    (
                        p.len() == 2 && vfo_of(&p[..1]) == Some(r.vfo),
                        p.chars().nth(1),
                    )
                } else {
                    (p.len() == 1, p.chars().next())
                };
                match code.map(|c| d.mode_of(c)) {
                    Some(m) if vfo_ok && m != Mode::Other => {
                        r.mode = m;
                        Vec::new()
                    }
                    _ => nak,
                }
            }
            ("IF", "") => self.info(d, if yaesu { Vfo::A } else { self.radio.vfo }),
            ("OI", "") if yaesu => self.info(d, Vfo::B),
            ("FR", "") if !yaesu => format!("FR{};", vfo_digit(r.vfo)).into_bytes(),
            ("FR", v) if !yaesu => match vfo_of(v) {
                Some(v) => {
                    r.vfo = v;
                    r.split = false;
                    Vec::new()
                }
                None => nak,
            },
            ("FT", v) if !yaesu && !v.is_empty() => match vfo_of(v) {
                Some(v) => {
                    r.split = v != r.vfo;
                    Vec::new()
                }
                None => nak,
            },
            ("FT", "") if d == Dialect::Yaesu => format!("FT{};", vfo_digit(r.vfo)).into_bytes(),
            ("FT", "2" | "3") if d == Dialect::Ftdx5000 => {
                r.split = rest == "3";
                Vec::new()
            }
            ("FR", "") if d == Dialect::Ftdx5000 => {
                format!("FR{};", if r.vfo == Vfo::A { 0 } else { 2 }).into_bytes()
            }
            ("FR", "") if d == Dialect::Ftdx101 => {
                format!("FR{};", if r.vfo == Vfo::A { "01" } else { "10" }).into_bytes()
            }
            ("VS", "") if yaesu => format!("VS{};", vfo_digit(r.vfo)).into_bytes(),
            ("VS", v) if yaesu => match vfo_of(v) {
                Some(v) => {
                    r.vfo = v;
                    Vec::new()
                }
                None => nak,
            },
            ("ST", "") if yaesu => format!("ST{};", r.split as u8).into_bytes(),
            ("ST", s) if yaesu && d != Dialect::Ftdx5000 => match bit(s) {
                Some(on) => {
                    r.split = on;
                    Vec::new()
                }
                None => nak,
            },
            ("TX", "") if yaesu => format!("TX{};", r.ptt as u8).into_bytes(),
            ("TX", s) if yaesu => match bit(s) {
                Some(on) => {
                    r.ptt = on;
                    Vec::new()
                }
                None => nak,
            },
            ("TX", "") => {
                r.ptt = true;
                Vec::new()
            }
            ("RX", "") if !yaesu => {
                r.ptt = false;
                Vec::new()
            }
            ("RT", "") => format!("RT{};", r.rit as u8).into_bytes(),
            ("XT", "") => format!("XT{};", r.xit as u8).into_bytes(),
            ("RT" | "XT", s) => match bit(s) {
                Some(on) if head == "RT" => {
                    r.rit = on;
                    Vec::new()
                }
                Some(on) => {
                    r.xit = on;
                    Vec::new()
                }
                None => nak,
            },
            ("RC", "") => {
                r.rit_offset = 0;
                Vec::new()
            }
            ("RU" | "RD", n) if n.len() == if d == Dialect::Kenwood { 5 } else { 4 } => {
                match n.parse::<i32>() {
                    Ok(n) if n >= 0 => {
                        let step = if head == "RU" { n } else { -n };
                        r.rit_offset = (r.rit_offset + step).clamp(-9999, 9999);
                        Vec::new()
                    }
                    _ => nak,
                }
            }
            ("RO", n) if d == Dialect::Elecraft && n.len() == 5 => match n.parse::<i32>() {
                Ok(n) => {
                    r.rit_offset = n;
                    Vec::new()
                }
                Err(_) => nak,
            },
            ("AI", s) => match s {
                "0" => {
                    r.auto_info = false;
                    Vec::new()
                }
                "1" | "2" | "3" => {
                    r.auto_info = true;
                    Vec::new()
                }
                _ => nak,
            },
            _ => nak,
        }
    }

    fn answer_binary(&mut self, cmd: &[u8]) -> Vec<u8> {
        let r = &mut self.radio;
        let p = [cmd[0], cmd[1], cmd[2], cmd[3]];
        match cmd[4] {
            yaesu::SET_FREQ => {
                if let Some(f) = yaesu::from_bcd(&p) {
                    r.set_freq(r.vfo, f * 10);
                }
            }
            yaesu::SET_MODE => {
                if let Some(m) = yaesu::mode_of(p[0]) {
                    r.mode = m;
                }
            }
            yaesu::READ_STATUS => {
                let mut b = yaesu::to_bcd(r.freq() / 10, 4);
                b.push(yaesu::mode_code(r.mode).unwrap_or(0x01));
                return b;
            }
            yaesu::PTT_ON | yaesu::PTT_OFF => r.ptt = cmd[4] == yaesu::PTT_ON,
            yaesu::SPLIT_ON | yaesu::SPLIT_OFF => r.split = cmd[4] == yaesu::SPLIT_ON,
            yaesu::CLAR_ON | yaesu::CLAR_OFF => r.rit = cmd[4] == yaesu::CLAR_ON,
            yaesu::TOGGLE_VFO => r.vfo = r.vfo.other(),
            yaesu::SET_CLAR => {
                if let Some(n) = yaesu::from_bcd(&p[2..]) {
                    let hz = n as i32 * 10;
                    r.rit_offset = if p[0] == 0 { hz } else { -hz };
                }
            }
            _ => {}
        }
        Vec::new()
    }
}

fn vfo_digit(vfo: Vfo) -> char {
    if vfo == Vfo::A {
        '0'
    } else {
        '1'
    }
}

fn ascii_code(d: Dialect, mode: Mode) -> char {
    d.mode_code(mode)
        .unwrap_or(if d.is_yaesu() { 'C' } else { '2' })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic() {
        let a: Vec<u64> = (0..4)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        let b: Vec<u64> = (0..4)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        assert_eq!(a, b);
        let mut r = Rng::new(1);
        assert!((0..1000).all(|_| r.unit() < 1.0));
        assert!(!r.chance(0.0));
    }

    #[test]
    fn latency_holds_replies_back() {
        let sim = Simulator::new(Family::Ascii(Dialect::Kenwood), Impairments::default(), 1);
        let mut t = sim.transport();
        let t0 = Instant::now();
        sim.advance(t0);
        t.write(b"FA;").unwrap();
        let mut buf = Vec::new();
        assert_eq!(t.read(&mut buf).unwrap(), 0);
        sim.advance(t0 + Duration::from_millis(5));
        t.read(&mut buf).unwrap();
        assert_eq!(buf, b"FA00007012000;");
    }

    #[test]
    fn icom_echo_and_collision() {
        let imp = Impairments {
            echo: true,
            latency: Duration::ZERO,
            ..Impairments::default()
        };
        let sim = Simulator::new(Family::Icom { address: 0x94 }, imp, 1);
        let mut t = sim.transport();
        let cmd = icom::frame(0x94, 0xE0, &[0x03]);
        t.write(&cmd).unwrap();
        let mut buf = Vec::new();
        t.read(&mut buf).unwrap();
        assert!(buf.starts_with(&cmd));
        assert!(buf.len() > cmd.len());
        sim.set_impairments(Impairments {
            collision: 1.0,
            ..imp
        });
        buf.clear();
        t.write(&cmd).unwrap();
        t.read(&mut buf).unwrap();
        assert_eq!(buf, [icom::JAM; 3]);
        assert_eq!(sim.stats().collisions, 1);
    }

    #[test]
    fn unsolicited_messages_arrive_on_schedule() {
        let imp = Impairments {
            transceive: Some(Duration::from_millis(100)),
            ..Impairments::default()
        };
        let sim = Simulator::new(Family::Icom { address: 0x94 }, imp, 1);
        let mut t = sim.transport();
        let t0 = Instant::now();
        sim.advance(t0 + Duration::from_millis(350));
        let mut buf = Vec::new();
        t.read(&mut buf).unwrap();
        assert_eq!(sim.stats().unsolicited, 3);
        assert_eq!(buf.iter().filter(|b| **b == icom::END).count(), 3);
    }

    #[test]
    fn every_ascii_mode_set_is_accepted_back() {
        for d in [Dialect::Kenwood, Dialect::Elecraft, Dialect::Yaesu] {
            for m in Mode::ALL {
                let Some(c) = d.mode_code(m) else { continue };
                assert_eq!(d.mode_of(c), m, "{d:?} {m:?}");
            }
        }
    }
}
