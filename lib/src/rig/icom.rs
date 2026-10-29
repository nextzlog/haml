use crate::rig::protocol::{Protocol, MAX_PENDING};
use crate::{Command, Event, Mode, Vfo};

pub const PREAMBLE: u8 = 0xFE;
pub const END: u8 = 0xFD;
pub const JAM: u8 = 0xFC;
pub const OK: u8 = 0xFB;
pub const NG: u8 = 0xFA;
pub const BROADCAST: u8 = 0x00;
pub const CONTROLLER: u8 = 0xE0;

pub const FREQ_RANGE: std::ops::RangeInclusive<u64> = 30_000..=250_000_000_000;

pub struct Icom {
    pub address: u8,
    pub controller: u8,
    buf: Vec<u8>,
    step: usize,
    vfo: Vfo,
    filters: [Option<u8>; 8],
}

impl Icom {
    pub fn new(address: u8) -> Self {
        Icom {
            address,
            controller: CONTROLLER,
            buf: Vec::new(),
            step: 0,
            vfo: Vfo::A,
            filters: [None; 8],
        }
    }

    pub fn frame(&self, body: &[u8]) -> Vec<u8> {
        frame(self.address, self.controller, body)
    }

    fn handle(&mut self, to: u8, from: u8, cmd: u8, data: &[u8], out: &mut Vec<Event>) {
        if from != self.address || (to != self.controller && to != BROADCAST) {
            return;
        }
        match (cmd, data) {
            (0x00 | 0x03, _) if matches!(data.len(), 5 | 6) => {
                if let Some(hz) = from_bcd(data).filter(|hz| FREQ_RANGE.contains(hz)) {
                    out.push(Event::Freq(self.vfo, hz));
                }
            }
            (0x01 | 0x04, [code, rest @ ..]) => {
                let mode = mode_of(*code);
                if let Some(w) = rest.first() {
                    self.filters[filter_slot(mode)] = Some(*w);
                }
                out.push(Event::Mode(mode));
            }
            (0x07, [v]) if *v <= 1 => {
                self.vfo = if *v == 0 { Vfo::A } else { Vfo::B };
                out.push(Event::Vfo(self.vfo));
            }
            (0x0F, [s]) if *s <= 1 => out.push(Event::Split(*s == 1)),
            (0x1C, [0x00, p]) if *p <= 1 => out.push(Event::Ptt(*p == 1)),
            (0x21, [0x00, lo, hi, sign]) => {
                if let Some(hz) = from_bcd(&[*lo, *hi]) {
                    let hz = hz as i32;
                    out.push(Event::RitOffset(if *sign == 1 { -hz } else { hz }));
                }
            }
            (0x21, [0x01, on]) if *on <= 1 => out.push(Event::Rit(*on == 1)),
            (0x21, [0x02, on]) if *on <= 1 => out.push(Event::Xit(*on == 1)),
            (OK, []) => out.push(Event::Ok),
            (NG, []) => out.push(Event::Rejected),
            _ => {}
        }
    }
}

pub fn frame(to: u8, from: u8, body: &[u8]) -> Vec<u8> {
    let mut f = vec![PREAMBLE, PREAMBLE, to, from];
    f.extend_from_slice(body);
    f.push(END);
    f
}

pub fn to_bcd(value: u64, bytes: usize) -> Vec<u8> {
    let mut n = value;
    (0..bytes)
        .map(|_| {
            let lo = (n % 10) as u8;
            n /= 10;
            let hi = (n % 10) as u8;
            n /= 10;
            (hi << 4) | lo
        })
        .collect()
}

pub fn from_bcd(bytes: &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for b in bytes.iter().rev() {
        let (hi, lo) = (b >> 4, b & 0x0F);
        if hi > 9 || lo > 9 {
            return None;
        }
        v = v.checked_mul(100)?.checked_add((hi * 10 + lo) as u64)?;
    }
    Some(v)
}

pub fn freq_bytes(hz: u64) -> usize {
    if hz >= 10_000_000_000 {
        6
    } else {
        5
    }
}

pub fn mode_of(code: u8) -> Mode {
    match code {
        0x00 => Mode::Lsb,
        0x01 => Mode::Usb,
        0x02 => Mode::Am,
        0x03 => Mode::Cw,
        0x04 => Mode::Rtty,
        0x05 | 0x06 => Mode::Fm,
        0x07 => Mode::CwR,
        0x08 => Mode::RttyR,
        0x17 => Mode::Dv,
        _ => Mode::Other,
    }
}

pub fn mode_code(mode: Mode) -> Option<u8> {
    Some(match mode {
        Mode::Lsb => 0x00,
        Mode::Usb => 0x01,
        Mode::Am => 0x02,
        Mode::Cw => 0x03,
        Mode::Rtty => 0x04,
        Mode::Fm => 0x05,
        Mode::CwR => 0x07,
        Mode::RttyR => 0x08,
        Mode::Dv => 0x17,
        Mode::Data | Mode::Other => return None,
    })
}

fn filter_slot(mode: Mode) -> usize {
    match mode {
        Mode::Lsb | Mode::Usb => 0,
        Mode::Cw | Mode::CwR => 1,
        Mode::Am => 2,
        Mode::Fm => 3,
        Mode::Rtty | Mode::RttyR => 4,
        Mode::Data => 5,
        Mode::Dv => 6,
        Mode::Other => 7,
    }
}

impl Protocol for Icom {
    fn encode(&mut self, command: &Command) -> Option<Vec<u8>> {
        let body = match *command {
            Command::SetFreq(_, hz) => {
                if hz >= 1_000_000_000_000 {
                    return None;
                }
                let mut b = vec![0x05];
                b.extend(to_bcd(hz, freq_bytes(hz)));
                b
            }
            Command::SetMode(_, mode) => {
                let mut b = vec![0x06, mode_code(mode)?];
                if let Some(w) = self.filters[filter_slot(mode)].filter(|w| (1..=3).contains(w)) {
                    b.push(w);
                }
                b
            }
            Command::SelectVfo(v) => {
                self.vfo = v;
                vec![0x07, if v == Vfo::A { 0x00 } else { 0x01 }]
            }
            Command::Split(on) => vec![0x0F, on as u8],
            Command::Ptt(on) => vec![0x1C, 0x00, on as u8],
            Command::Rit(on) => vec![0x21, 0x01, on as u8],
            Command::Xit(on) => vec![0x21, 0x02, on as u8],
            Command::RitOffset(hz) => {
                if hz.unsigned_abs() > 9999 {
                    return None;
                }
                let mut b = vec![0x21, 0x00];
                b.extend(to_bcd(hz.unsigned_abs() as u64, 2));
                b.push((hz < 0) as u8);
                b
            }
            Command::ClearRit => vec![0x21, 0x00, 0x00, 0x00, 0x00],
            Command::Transceive(_) => return None,
        };
        Some(self.frame(&body))
    }

    fn poll(&mut self) -> Vec<u8> {
        self.step = (self.step + 1) % 2;
        self.frame(&[if self.step == 1 { 0x03 } else { 0x04 }])
    }

    fn decode(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let Some(first) = self.buf.windows(2).position(|w| w == [PREAMBLE; 2]) else {
                let keep = self.buf.last() == Some(&PREAMBLE);
                self.buf.clear();
                if keep {
                    self.buf.push(PREAMBLE);
                }
                break;
            };
            let Some(end) = self.buf[first..].iter().position(|b| *b == END) else {
                self.buf.drain(..first);
                if self.buf.len() > MAX_PENDING {
                    self.buf.clear();
                }
                break;
            };
            let end = first + end;
            let start = first
                + self.buf[first..end]
                    .windows(2)
                    .rposition(|w| w == [PREAMBLE; 2])
                    .unwrap_or(0);
            let frame = self.buf[start + 2..end].to_vec();
            self.buf.drain(..=end);
            if frame.len() < 3 || frame.contains(&JAM) {
                continue;
            }
            self.handle(frame[0], frame[1], frame[2], &frame[3..], &mut out);
        }
        out
    }

    fn acknowledges(&self) -> bool {
        true
    }

    fn verify(&mut self, command: &Command) -> Option<Vec<u8>> {
        let body: &[u8] = match command {
            Command::SetFreq(..) => &[0x03],
            Command::SetMode(..) => &[0x04],
            Command::Split(_) => &[0x0F],
            Command::Ptt(_) => &[0x1C, 0x00],
            Command::Rit(_) => &[0x21, 0x01],
            Command::Xit(_) => &[0x21, 0x02],
            _ => return None,
        };
        Some(self.frame(body))
    }

    fn reset(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcd_round_trip() {
        assert_eq!(to_bcd(14_025_000, 5), vec![0x00, 0x50, 0x02, 0x14, 0x00]);
        assert_eq!(from_bcd(&[0x00, 0x50, 0x02, 0x14, 0x00]), Some(14_025_000));
        assert_eq!(from_bcd(&[0xAA]), None);
        assert_eq!(from_bcd(&to_bcd(10_368_100_000, 6)), Some(10_368_100_000));
    }

    #[test]
    fn preamble_split_across_reads() {
        let mut p = Icom::new(0x94);
        assert!(p.decode(&[0x12, 0xFE]).is_empty());
        assert_eq!(
            p.decode(&[0xFE, 0xE0, 0x94, 0x03, 0x00, 0x50, 0x02, 0x14, 0x00, 0xFD]),
            vec![Event::Freq(Vfo::A, 14_025_000)]
        );
        assert!(p.decode(&[0xFE, 0xFE, 0xE0, 0x94, 0x03, 0xFD]).is_empty());
    }

    #[test]
    fn truncated_frame_does_not_swallow_the_next() {
        let mut p = Icom::new(0x94);
        let mut b = vec![0xFE, 0xFE, 0xE0, 0x94, 0x03, 0x00];
        b.extend([
            0xFE, 0xFE, 0xFE, 0xE0, 0x94, 0x03, 0x00, 0x50, 0x02, 0x14, 0x00, 0xFD,
        ]);
        assert_eq!(p.decode(&b), vec![Event::Freq(Vfo::A, 14_025_000)]);
    }

    #[test]
    fn echo_other_addresses_and_jam_are_ignored() {
        let mut p = Icom::new(0x94);
        let echo = p.encode(&Command::SetFreq(Vfo::A, 14_025_000)).unwrap();
        assert_eq!(
            echo,
            vec![0xFE, 0xFE, 0x94, 0xE0, 0x05, 0x00, 0x50, 0x02, 0x14, 0x00, 0xFD]
        );
        assert!(p.decode(&echo).is_empty());
        assert!(p.decode(&[0xFE, 0xFE, 0xE0, 0x98, 0xFB, 0xFD]).is_empty());
        assert!(p.decode(&[0xFE, 0xFE, 0xFC, 0xFC, 0xFC, 0xFD]).is_empty());
        assert_eq!(p.decode(&[0xFE, 0xFE, 0xE0, 0x94, 0xFB, 0xFD]), [Event::Ok]);
        assert_eq!(
            p.decode(&[0xFE, 0xFE, 0x00, 0x94, 0x01, 0x03, 0x01, 0xFD]),
            [Event::Mode(Mode::Cw)]
        );
    }

    #[test]
    fn keeps_the_filter_of_each_mode() {
        let mut p = Icom::new(0x94);
        let usb = Command::SetMode(Vfo::A, Mode::Usb);
        assert_eq!(
            p.encode(&usb).unwrap(),
            [0xFE, 0xFE, 0x94, 0xE0, 0x06, 0x01, 0xFD]
        );
        p.decode(&[0xFE, 0xFE, 0xE0, 0x94, 0x04, 0x00, 0x02, 0xFD]);
        assert_eq!(p.encode(&usb).unwrap()[6], 0x02);
        assert!(p.encode(&Command::SetMode(Vfo::A, Mode::Data)).is_none());
    }

    #[test]
    fn rit_split_ptt_and_vfo() {
        let mut p = Icom::new(0x94);
        let body = |p: &mut Icom, c| {
            let f = p.encode(&c).unwrap();
            f[4..f.len() - 1].to_vec()
        };
        assert_eq!(
            body(&mut p, Command::RitOffset(-1234)),
            [0x21, 0x00, 0x34, 0x12, 0x01]
        );
        assert_eq!(body(&mut p, Command::Split(true)), [0x0F, 0x01]);
        assert_eq!(body(&mut p, Command::Ptt(true)), [0x1C, 0x00, 0x01]);
        assert_eq!(body(&mut p, Command::SelectVfo(Vfo::B)), [0x07, 0x01]);
        assert!(p.encode(&Command::RitOffset(10_000)).is_none());
        assert!(p.encode(&Command::SetFreq(Vfo::A, u64::MAX)).is_none());
        assert_eq!(
            p.decode(&[0xFE, 0xFE, 0xE0, 0x94, 0x21, 0x00, 0x34, 0x12, 0x01, 0xFD]),
            [Event::RitOffset(-1234)]
        );
        assert_eq!(
            p.decode(&[0xFE, 0xFE, 0xE0, 0x94, 0x03, 0x00, 0x50, 0x02, 0x14, 0x00, 0xFD]),
            [Event::Freq(Vfo::B, 14_025_000)]
        );
    }

    #[test]
    fn malformed_frames_and_bounded_buffer() {
        let mut p = Icom::new(0x94);
        assert!(p.decode(&[]).is_empty());
        assert!(p.decode(&[0xFE, 0xFE, 0xFD]).is_empty());
        assert!(p.decode(&[0xFE, 0xFE, 0xE0, 0x94, 0x04, 0xFD]).is_empty());
        assert!(p
            .decode(&[0xFE, 0xFE, 0xE0, 0x94, 0x03, 0x00, 0x00, 0xFD])
            .is_empty());
        let mut noise = vec![0xFE, 0xFE];
        noise.extend([0x11; 400]);
        assert!(p.decode(&noise).is_empty());
        assert!(p.buf.len() <= MAX_PENDING);
    }
}
