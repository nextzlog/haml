use std::collections::VecDeque;
use std::io;
use std::thread;
use std::time::{Duration, Instant};

use crate::keyer::{Keyer, KeyerCaps, KeyerEvent};
use crate::morse::{MAX_WPM, MIN_WPM};
use crate::transport::{Boxed, Shared, Slot, Transport};

pub const ADMIN: u8 = 0x00;
pub const ADMIN_CAL: u8 = 0x00;
pub const ADMIN_OPEN: u8 = 0x02;
pub const ADMIN_CLOSE: u8 = 0x03;
pub const ADMIN_SET_WK2_MODE: u8 = 11;
pub const SIDETONE: u8 = 0x01;
pub const SET_WPM: u8 = 0x02;
pub const SET_PTT_DELAY: u8 = 0x04;
pub const SET_SPEEDPOT: u8 = 0x05;
pub const GET_SPEEDPOT: u8 = 0x07;
pub const SET_PINCFG: u8 = 0x09;
pub const CLEAR: u8 = 0x0a;
pub const SET_MODE: u8 = 0x0e;
pub const MODE_SERIAL_ECHOBACK: u8 = 0x04;
pub const MODE_PADDLE_SWAP: u8 = 0x08;
pub const PTT: u8 = 0x18;
pub const MERGE: u8 = 0x1b;
pub const BUFFERED_SPEED: u8 = 0x1c;
pub const STATUS_BREAKIN: u8 = 0x02;
pub const STATUS_BUSY: u8 = 0x04;
pub const POT_MIN_WPM: u8 = 5;
pub const REVISION_TIMEOUT: Duration = Duration::from_secs(1);
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    pub wpm: u32,
    pub paddle_swap: bool,
    pub sidetone: bool,
    pub ptt_before_ms: u32,
    pub ptt_after_ms: u32,
    pub speed_pot: bool,
}

#[cfg(test)]
fn open_commands(s: &Setup) -> Vec<(Vec<u8>, u64)> {
    let mut cmds = setup_commands(s);
    cmds.extend(after_revision_commands(s));
    cmds
}

fn setup_commands(s: &Setup) -> Vec<(Vec<u8>, u64)> {
    let mut mode = MODE_SERIAL_ECHOBACK;
    if s.paddle_swap {
        mode |= MODE_PADDLE_SWAP;
    }
    vec![
        (vec![ADMIN, ADMIN_CAL], 100),
        (vec![0xff], 0),
        (vec![ADMIN, ADMIN_OPEN], 50),
        (vec![ADMIN, ADMIN_SET_WK2_MODE], 50),
        (vec![SET_MODE, mode], 50),
        (vec![SET_SPEEDPOT, POT_MIN_WPM, 50, 0], 0),
    ]
}

fn after_revision_commands(s: &Setup) -> Vec<(Vec<u8>, u64)> {
    let mut pincfg = 0xa0 | 0x01 | 0x0c;
    if s.sidetone {
        pincfg |= 0x02;
    }
    let ms10 = |ms: u32| (ms / 10).min(250) as u8;
    let pot = s.speed_pot.then(|| (vec![GET_SPEEDPOT], 50));
    pot.into_iter()
        .chain([
            (vec![SET_PINCFG, pincfg], 50),
            (vec![PTT, 0], 50),
            (
                vec![SET_PTT_DELAY, ms10(s.ptt_before_ms), ms10(s.ptt_after_ms)],
                50,
            ),
            (vec![SET_WPM, s.wpm as u8], 0),
            (vec![SIDETONE, if s.sidetone { 0x06 } else { 0x86 }], 50),
        ])
        .collect()
}

pub fn build_message(text: &str, wpm: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut wpm = wpm;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let merged = match c {
            'a' => Some("AR"),
            'b' | '~' => Some("BK"),
            't' => Some("BT"),
            'k' => Some("KN"),
            's' => Some("SK"),
            'v' => Some("VA"),
            _ => None,
        };
        if let Some(pair) = merged {
            out.push(MERGE);
            out.extend_from_slice(pair.as_bytes());
            continue;
        }
        match c {
            '\\' => {
                let sign = chars.get(i).copied();
                let n = chars.get(i + 1).and_then(|d| d.to_digit(10)).unwrap_or(0);
                if n > 0 && matches!(sign, Some('+') | Some('-')) {
                    wpm = if sign == Some('+') {
                        wpm + n
                    } else {
                        wpm.saturating_sub(n)
                    };
                    out.extend_from_slice(&[BUFFERED_SPEED, wpm.clamp(MIN_WPM, 99) as u8]);
                    i += 2;
                }
            }
            '_' => out.push(b'|'),
            '.' => out.push(b'?'),
            ' ' | 'A'..='Z' | '0'..='9' | '/' | '?' | '-' | '=' => out.push(c as u8),
            _ => {}
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Status(u8),
    SpeedPot(u8),
    Pushbutton(u8),
    Echo(u8),
}

pub fn parse_reply(b: u8) -> Reply {
    if b & 0xc8 == 0xc8 {
        return Reply::Pushbutton(b);
    }
    match b & 0xc0 {
        0xc0 => Reply::Status(b),
        0x80 => Reply::SpeedPot(b & 0x3f),
        _ => Reply::Echo(b),
    }
}

pub struct WinKeyer<W: Transport> {
    wire: W,
    pending: VecDeque<u8>,
    slot: Option<Slot>,
    wpm: u32,
    status: u8,
    sending: bool,
    sent_at: Option<Instant>,
    failed: bool,
}

impl<W: Transport + Send> WinKeyer<W> {
    pub fn open(mut wire: W, setup: &Setup) -> io::Result<Self> {
        thread::sleep(Duration::from_millis(500));
        let mut buf = Vec::new();
        while wire.read(&mut buf)? > 0 {}
        for (bytes, pause) in setup_commands(setup) {
            wire.write(&bytes)?;
            thread::sleep(Duration::from_millis(pause));
        }
        let start = Instant::now();
        let mut revision = Vec::new();
        while wire.read(&mut revision)? == 0 {
            if start.elapsed() >= REVISION_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no answer from WinKeyer",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        for (bytes, pause) in after_revision_commands(setup) {
            wire.write(&bytes)?;
            thread::sleep(Duration::from_millis(pause));
        }
        let mut k = Self::new(wire, setup.wpm);
        k.pending.extend(revision.into_iter().skip(1));
        Ok(k)
    }

    pub fn new(wire: W, wpm: u32) -> Self {
        WinKeyer {
            wire,
            pending: VecDeque::new(),
            slot: None,
            wpm,
            status: 0,
            sending: false,
            sent_at: None,
            failed: false,
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Option<KeyerEvent> {
        if self.failed {
            return None;
        }
        match self.wire.write(bytes) {
            Ok(()) => None,
            Err(e) => {
                self.failed = true;
                Some(KeyerEvent::Error(e.to_string()))
            }
        }
    }
}

impl<W: Transport + Send> WinKeyer<W> {
    fn waiting(&self) -> bool {
        self.sending && self.sent_at.is_some_and(|t| t.elapsed() < BUSY_TIMEOUT)
    }
}

fn keyable(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            BUFFERED_SPEED => i += 2,
            b' ' | b'|' => i += 1,
            _ => return true,
        }
    }
    false
}

impl WinKeyer<Shared> {
    pub fn publish(port: Boxed, setup: &Setup, slot: &Slot) -> io::Result<Self> {
        let shared = Shared::new(port);
        slot.publish(shared.clone());
        match WinKeyer::open(shared, setup) {
            Ok(mut k) => {
                k.slot = Some(slot.clone());
                Ok(k)
            }
            Err(e) => {
                slot.withdraw();
                Err(e)
            }
        }
    }
}

impl<W: Transport> Drop for WinKeyer<W> {
    fn drop(&mut self) {
        if let Some(slot) = &self.slot {
            slot.withdraw();
        }
        let _ = self.wire.write(&[PTT, 0]);
        let _ = self.wire.write(&[ADMIN, ADMIN_CLOSE]);
    }
}

impl<W: Transport + Send> Keyer for WinKeyer<W> {
    fn caps(&self) -> KeyerCaps {
        KeyerCaps {
            ptt: true,
            ptt_delays: true,
            paddle: true,
            speed_pot: true,
            fsk: false,
        }
    }

    fn send(&mut self, text: &str) {
        let bytes = build_message(text, self.wpm);
        if !bytes.is_empty() {
            if keyable(&bytes) {
                self.sending = true;
                self.sent_at = Some(Instant::now());
            }
            let _ = self.write(&bytes);
        }
    }

    fn abort(&mut self) {
        self.sending = false;
        self.status &= !STATUS_BUSY;
        let _ = self.write(&[CLEAR]);
    }

    fn set_wpm(&mut self, wpm: u32) {
        self.wpm = wpm.clamp(MIN_WPM, MAX_WPM);
        let _ = self.write(&[SET_WPM, self.wpm as u8]);
    }

    fn wpm(&self) -> u32 {
        self.wpm
    }

    fn set_ptt(&mut self, on: bool) {
        let _ = self.write(&[PTT, on as u8]);
    }

    fn busy(&self) -> bool {
        self.waiting() || self.status & STATUS_BUSY != 0
    }

    fn poll(&mut self) -> Vec<KeyerEvent> {
        let mut events = Vec::new();
        if self.sending && !self.waiting() {
            self.sending = false;
            if self.status & STATUS_BUSY == 0 {
                events.push(KeyerEvent::Finished);
            }
        }
        if self.failed {
            return events;
        }
        let mut buf: Vec<u8> = self.pending.drain(..).collect();
        loop {
            let n = match self.failed {
                true => 0,
                false => match self.wire.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        self.failed = true;
                        events.push(KeyerEvent::Error(e.to_string()));
                        0
                    }
                },
            };
            if n == 0 && buf.is_empty() {
                break;
            }
            for b in std::mem::take(&mut buf) {
                match parse_reply(b) {
                    Reply::Status(s) => {
                        if (s ^ self.status) & STATUS_BREAKIN != 0 {
                            events.push(KeyerEvent::Paddle(s & STATUS_BREAKIN != 0));
                        }
                        let was = self.status & STATUS_BUSY != 0;
                        if s & STATUS_BUSY != 0 {
                            self.sending = false;
                        }
                        self.status = s;
                        if was && s & STATUS_BUSY == 0 {
                            self.sending = false;
                            events.push(KeyerEvent::Finished);
                        }
                    }
                    Reply::SpeedPot(p) => {
                        let wpm = (POT_MIN_WPM as u32 + p as u32).clamp(MIN_WPM, MAX_WPM);
                        self.wpm = wpm;
                        if let Some(e) = self.write(&[SET_WPM, wpm as u8]) {
                            events.push(e);
                        }
                        events.push(KeyerEvent::Speed(wpm));
                    }
                    Reply::Echo(_) | Reply::Pushbutton(_) => {}
                }
            }
        }
        events
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct FakeWire {
        pub written: Arc<Mutex<Vec<u8>>>,
        pub replies: Arc<Mutex<VecDeque<u8>>>,
        pub revision: Option<u8>,
    }

    impl Transport for FakeWire {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.written.lock().unwrap().extend_from_slice(bytes);
            if bytes == [ADMIN, ADMIN_OPEN] {
                if let Some(r) = self.revision {
                    self.replies.lock().unwrap().push_back(r);
                }
            }
            Ok(())
        }

        fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
            let mut r = self.replies.lock().unwrap();
            let n = r.len();
            buf.extend(r.drain(..));
            Ok(n)
        }
    }

    #[test]
    fn message_bytes() {
        assert_eq!(build_message("TU a", 25), b"TU \x1bAR".to_vec());
        assert_eq!(build_message("5NN_1.(@)", 25), b"5NN|1?".to_vec());
        assert_eq!(
            build_message("\\+3CQ\\-1", 25),
            vec![0x1c, 28, b'C', b'Q', 0x1c, 27]
        );
        assert_eq!(
            build_message("~kts", 25),
            b"\x1bBK\x1bKN\x1bBT\x1bSK".to_vec()
        );
    }

    #[test]
    fn pause_and_speed_pass_through() {
        assert_eq!(build_message("TU^ u]", 25), b"TU ".to_vec());
        assert_eq!(build_message("", 25), Vec::<u8>::new());
        assert_eq!(build_message(":*@\"", 25), Vec::<u8>::new());
    }

    #[test]
    fn replies() {
        assert_eq!(parse_reply(0xc4), Reply::Status(0xc4));
        assert_eq!(parse_reply(0x8a), Reply::SpeedPot(10));
        assert_eq!(parse_reply(b'A'), Reply::Echo(b'A'));
        assert_eq!(parse_reply(0xc8), Reply::Pushbutton(0xc8));
        assert_eq!(parse_reply(0xcc), Reply::Pushbutton(0xcc));
    }

    #[test]
    fn open_sequence() {
        let cmds = open_commands(&Setup {
            wpm: 30,
            paddle_swap: true,
            sidetone: false,
            ptt_before_ms: 50,
            ptt_after_ms: 120,
            speed_pot: true,
        });
        assert_eq!(cmds[0].0, vec![0, 0]);
        assert!(cmds.contains(&(vec![SET_MODE, 0x0c], 50)));
        assert!(cmds.contains(&(vec![SET_PINCFG, 0xad], 50)));
        assert!(cmds.contains(&(vec![SET_PTT_DELAY, 5, 12], 50)));
        assert!(cmds.contains(&(vec![SET_WPM, 30], 0)));
        assert!(cmds.contains(&(vec![SIDETONE, 0x86], 50)));
        assert!(cmds.contains(&(vec![GET_SPEEDPOT], 50)));
    }

    #[test]
    fn with_so2r_neo_the_speed_pot_is_not_read() {
        let setup = Setup {
            wpm: 30,
            paddle_swap: false,
            sidetone: true,
            ptt_before_ms: 50,
            ptt_after_ms: 120,
            speed_pot: false,
        };
        assert!(!open_commands(&setup)
            .iter()
            .any(|(c, _)| c[0] == GET_SPEEDPOT));
    }

    #[test]
    fn busy_until_idle_status() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        k.send("CQ");
        assert!(k.busy());
        wire.replies.lock().unwrap().extend([0xc4, b'C', 0x85]);
        assert_eq!(k.poll(), vec![KeyerEvent::Speed(10)]);
        assert_eq!(k.wpm(), 10);
        assert!(k.busy());
        wire.replies.lock().unwrap().extend([0xc2, 0xc0]);
        assert_eq!(
            k.poll(),
            vec![
                KeyerEvent::Paddle(true),
                KeyerEvent::Finished,
                KeyerEvent::Paddle(false)
            ]
        );
        assert!(!k.busy());
        k.abort();
        k.set_ptt(true);
        drop(k);
        let w = wire.written.lock().unwrap().clone();
        assert_eq!(w, b"CQ\x02\x0a\x0a\x18\x01\x18\x00\x00\x03".to_vec());
    }

    #[test]
    fn idle_status_of_a_stopped_message_does_not_end_the_next() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        k.send("CQ CQ");
        wire.replies.lock().unwrap().push_back(0xc4);
        assert!(k.poll().is_empty());
        k.abort();
        k.send("TU");
        wire.replies.lock().unwrap().push_back(0xc0);
        assert!(k.poll().is_empty());
        assert!(k.busy());
        wire.replies.lock().unwrap().push_back(0xc4);
        assert!(k.poll().is_empty());
        wire.replies.lock().unwrap().push_back(0xc0);
        assert_eq!(k.poll(), vec![KeyerEvent::Finished]);
        assert!(!k.busy());
    }

    #[test]
    fn pushbutton_does_not_end_a_message() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        k.send("CQ");
        wire.replies.lock().unwrap().extend([0xc4, 0xc8]);
        assert_eq!(k.poll(), vec![]);
        assert!(k.busy());
        wire.replies.lock().unwrap().push_back(0xc0);
        assert_eq!(k.poll(), vec![KeyerEvent::Finished]);
    }

    #[test]
    fn speed_pot_sets_the_keyer_speed() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        wire.replies.lock().unwrap().extend([0x80, 0x99]);
        assert_eq!(k.poll(), vec![KeyerEvent::Speed(5), KeyerEvent::Speed(30)]);
        assert_eq!(*wire.written.lock().unwrap(), vec![SET_WPM, 5, SET_WPM, 30]);
        assert_eq!(k.wpm(), 30);
    }

    #[test]
    fn speed_pot_beyond_the_range_is_clamped() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        wire.replies.lock().unwrap().push_back(0xbf);
        assert_eq!(k.poll(), vec![KeyerEvent::Speed(MAX_WPM)]);
        assert_eq!(*wire.written.lock().unwrap(), vec![SET_WPM, MAX_WPM as u8]);
    }

    #[test]
    fn ptt_delays_in_ten_ms_steps() {
        let setup = Setup {
            wpm: 25,
            paddle_swap: false,
            sidetone: true,
            ptt_before_ms: 1,
            ptt_after_ms: 255,
            speed_pot: true,
        };
        assert!(open_commands(&setup).contains(&(vec![SET_PTT_DELAY, 0, 25], 50)));
    }

    #[test]
    fn open_waits_for_the_revision_byte() {
        let setup = Setup {
            wpm: 25,
            paddle_swap: false,
            sidetone: true,
            ptt_before_ms: 25,
            ptt_after_ms: 1,
            speed_pot: true,
        };
        let silent = FakeWire::default();
        let e = WinKeyer::open(silent.clone(), &setup).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(!silent.written.lock().unwrap().contains(&GET_SPEEDPOT));
        let wire = FakeWire {
            revision: Some(23),
            ..FakeWire::default()
        };
        let mut k = WinKeyer::open(wire.clone(), &setup).unwrap();
        let w = wire.written.lock().unwrap().clone();
        let all: Vec<u8> = open_commands(&setup)
            .into_iter()
            .flat_map(|c| c.0)
            .collect();
        assert_eq!(w, all);
        assert!(k.poll().is_empty());
        assert!(wire.replies.lock().unwrap().is_empty());
    }

    #[test]
    fn busy_ends_without_a_busy_status() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        k.send("E");
        assert!(k.busy());
        assert!(k.poll().is_empty());
        thread::sleep(BUSY_TIMEOUT + Duration::from_millis(100));
        assert!(!k.busy());
        assert_eq!(k.poll(), vec![KeyerEvent::Finished]);
        assert!(k.poll().is_empty());
    }

    #[test]
    fn busy_status_outlasts_the_timeout() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        k.send("CQ CQ");
        wire.replies.lock().unwrap().push_back(0xc4);
        assert!(k.poll().is_empty());
        thread::sleep(BUSY_TIMEOUT + Duration::from_millis(100));
        assert!(k.busy());
        assert!(k.poll().is_empty());
        wire.replies.lock().unwrap().push_back(0xc0);
        assert_eq!(k.poll(), vec![KeyerEvent::Finished]);
        assert!(!k.busy());
    }

    #[test]
    fn nothing_to_key_is_not_busy() {
        let wire = FakeWire::default();
        let mut k = WinKeyer::new(wire.clone(), 25);
        k.send("");
        k.send("@#");
        assert!(wire.written.lock().unwrap().is_empty());
        k.send("  _\\+3 ");
        assert!(!k.busy());
        assert!(k.poll().is_empty());
        assert!(wire.written.lock().unwrap().contains(&BUFFERED_SPEED));
    }

    #[test]
    fn keyable_bytes() {
        assert!(!keyable(b""));
        assert!(!keyable(b"  |"));
        assert!(!keyable(&[BUFFERED_SPEED, b'E']));
        assert!(keyable(&[BUFFERED_SPEED, 30, b'E']));
        assert!(keyable(b" ?"));
        assert!(keyable(&[MERGE, b'A', b'R']));
    }

    #[derive(Default)]
    struct Answering {
        written: Vec<u8>,
        replies: Vec<u8>,
    }

    impl crate::transport::Emulator for Answering {
        fn receive(&mut self, bytes: &[u8]) {
            self.written.extend_from_slice(bytes);
            if bytes == [ADMIN, ADMIN_OPEN] {
                self.replies.push(23);
            }
        }
        fn send(&mut self, out: &mut Vec<u8>) {
            out.append(&mut self.replies);
        }
    }

    fn defaults() -> Setup {
        Setup {
            wpm: 25,
            paddle_swap: false,
            sidetone: true,
            ptt_before_ms: 25,
            ptt_after_ms: 1,
            speed_pot: true,
        }
    }

    #[test]
    fn the_port_is_shared_while_the_keyer_is_open() {
        let port = crate::transport::Emulated::new(Answering::default());
        let slot = Slot::default();
        let mut k = WinKeyer::publish(Box::new(port.clone()), &defaults(), &slot).unwrap();
        port.with(|p| p.written.clear());
        let mut other = slot.get().expect("published");
        other.write(&[0x94, 0x95]).unwrap();
        k.send("E");
        assert_eq!(port.with(|p| p.written.clone()), [0x94, 0x95, b'E']);
        drop(k);
        assert!(slot.get().is_none());
        let w = port.with(|p| p.written.clone());
        assert!(w.ends_with(&[PTT, 0, ADMIN, ADMIN_CLOSE]));
    }

    #[test]
    fn a_keyer_that_does_not_answer_is_not_published() {
        let (mute, _peer) = crate::transport::Loop::pair();
        let slot = Slot::default();
        assert!(WinKeyer::publish(Box::new(mute), &defaults(), &slot).is_err());
        assert!(slot.get().is_none());
    }
}
