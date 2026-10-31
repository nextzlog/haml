use crate::rig::protocol::Protocol;
use crate::{Command, Event, Mode, Vfo};

pub const SET_FREQ: u8 = 0x01;
pub const SPLIT_ON: u8 = 0x02;
pub const READ_STATUS: u8 = 0x03;
pub const CLAR_ON: u8 = 0x05;
pub const SET_MODE: u8 = 0x07;
pub const PTT_ON: u8 = 0x08;
pub const TOGGLE_VFO: u8 = 0x81;
pub const SPLIT_OFF: u8 = 0x82;
pub const CLAR_OFF: u8 = 0x85;
pub const PTT_OFF: u8 = 0x88;
pub const SET_CLAR: u8 = 0xF5;

pub struct YaesuBinary {
    buf: Vec<u8>,
    vfo: Vfo,
}

impl Default for YaesuBinary {
    fn default() -> Self {
        Self::new()
    }
}

impl YaesuBinary {
    pub fn new() -> Self {
        YaesuBinary {
            buf: Vec::new(),
            vfo: Vfo::A,
        }
    }
}

pub fn to_bcd(value: u64, bytes: usize) -> Vec<u8> {
    let mut v = crate::rig::icom::to_bcd(value, bytes);
    v.reverse();
    v
}

pub fn from_bcd(bytes: &[u8]) -> Option<u64> {
    let v: Vec<u8> = bytes.iter().rev().copied().collect();
    crate::rig::icom::from_bcd(&v)
}

pub fn mode_of(code: u8) -> Option<Mode> {
    Some(match code {
        0x00 => Mode::Lsb,
        0x01 => Mode::Usb,
        0x02 => Mode::Cw,
        0x03 => Mode::CwR,
        0x04 => Mode::Am,
        0x06 | 0x08 | 0x88 => Mode::Fm,
        0x0A => Mode::Data,
        0x0C => Mode::Data,
        0x82 => Mode::Cw,
        0x83 => Mode::CwR,
        _ => return None,
    })
}

pub fn mode_code(mode: Mode) -> Option<u8> {
    Some(match mode {
        Mode::Lsb => 0x00,
        Mode::Usb => 0x01,
        Mode::Cw => 0x02,
        Mode::CwR => 0x03,
        Mode::Am => 0x04,
        Mode::Fm => 0x08,
        Mode::Data => 0x0A,
        _ => return None,
    })
}

fn op(p: [u8; 4], code: u8) -> Vec<u8> {
    vec![p[0], p[1], p[2], p[3], code]
}

impl Protocol for YaesuBinary {
    fn encode(&mut self, command: &Command) -> Option<Vec<u8>> {
        Some(match *command {
            Command::SetFreq(vfo, hz) => {
                if vfo != self.vfo || hz >= 1_000_000_000 || hz % 10 != 0 {
                    return None;
                }
                let mut b = to_bcd(hz / 10, 4);
                b.push(SET_FREQ);
                b
            }
            Command::SetMode(vfo, mode) => {
                if vfo != self.vfo {
                    return None;
                }
                op([mode_code(mode)?, 0, 0, 0], SET_MODE)
            }
            Command::SelectVfo(vfo) => {
                if vfo == self.vfo {
                    return None;
                }
                self.vfo = vfo;
                op([0; 4], TOGGLE_VFO)
            }
            Command::Split(on) => op([0; 4], if on { SPLIT_ON } else { SPLIT_OFF }),
            Command::Ptt(on) => op([0; 4], if on { PTT_ON } else { PTT_OFF }),
            Command::Rit(on) => op([0; 4], if on { CLAR_ON } else { CLAR_OFF }),
            Command::RitOffset(hz) => {
                let n = hz.unsigned_abs() as u64 / 10;
                if n > 9999 {
                    return None;
                }
                let b = to_bcd(n, 2);
                op([(hz < 0) as u8, 0, b[0], b[1]], SET_CLAR)
            }
            Command::ClearRit => op([0; 4], SET_CLAR),
            Command::Xit(_) | Command::Transceive(_) => return None,
        })
    }

    fn poll(&mut self) -> Vec<u8> {
        op([0; 4], READ_STATUS)
    }

    fn decode(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buf.len() >= 5 {
            let reply = &self.buf[..5];
            match (from_bcd(&reply[..4]), mode_of(reply[4])) {
                (Some(f), Some(m)) if f > 0 => {
                    out.push(Event::Freq(self.vfo, f * 10));
                    out.push(Event::Mode(m));
                    self.buf.drain(..5);
                }
                _ => {
                    self.buf.remove(0);
                }
            }
        }
        out
    }

    fn caps(&self) -> crate::RigCaps {
        crate::RigCaps {
            xit: false,
            transceive: false,
            ..crate::RigCaps::ALL
        }
    }

    fn acknowledges(&self) -> bool {
        false
    }

    fn verify(&mut self, command: &Command) -> Option<Vec<u8>> {
        match command {
            Command::SetFreq(..) | Command::SetMode(..) => Some(self.poll()),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_and_mode() {
        let mut y = YaesuBinary::new();
        assert_eq!(
            y.encode(&Command::SetFreq(Vfo::A, 7_035_000)).unwrap(),
            [0x00, 0x70, 0x35, 0x00, 0x01]
        );
        assert_eq!(
            y.encode(&Command::SetMode(Vfo::A, Mode::Usb)).unwrap(),
            [0x01, 0, 0, 0, 0x07]
        );
        assert!(y.encode(&Command::SetMode(Vfo::A, Mode::Rtty)).is_none());
        assert!(y.encode(&Command::SetFreq(Vfo::B, 7_035_000)).is_none());
        assert!(y.encode(&Command::SetFreq(Vfo::A, 7_035_005)).is_none());
        assert_eq!(
            y.decode(&[0x00, 0x70, 0x35, 0x00, 0x82]),
            [Event::Freq(Vfo::A, 7_035_000), Event::Mode(Mode::Cw)]
        );
    }

    #[test]
    fn partial_reads_and_reset() {
        let mut y = YaesuBinary::new();
        assert!(y.decode(&[0x01, 0x40]).is_empty());
        let r = y.decode(&[0x25, 0x00, 0x01]);
        assert_eq!(r, [Event::Freq(Vfo::A, 14_025_000), Event::Mode(Mode::Usb)]);
        assert!(y.decode(&[0xFF, 0x14]).is_empty());
        y.reset();
        let r = y.decode(&[0x01, 0x40, 0x25, 0x00, 0x01]);
        assert_eq!(r.len(), 2);
        assert!(y.decode(&[0xAA; 9]).is_empty());
        assert!(y.buf.len() < 5);
    }

    #[test]
    fn vfo_split_ptt_clarifier() {
        let mut y = YaesuBinary::new();
        assert_eq!(
            y.encode(&Command::SelectVfo(Vfo::B)).unwrap()[4],
            TOGGLE_VFO
        );
        assert!(y.encode(&Command::SelectVfo(Vfo::B)).is_none());
        assert_eq!(y.encode(&Command::Ptt(true)).unwrap()[4], PTT_ON);
        assert_eq!(y.encode(&Command::Split(false)).unwrap()[4], SPLIT_OFF);
        assert_eq!(
            y.encode(&Command::RitOffset(-1230)).unwrap(),
            [0x01, 0x00, 0x01, 0x23, SET_CLAR]
        );
        assert!(y.encode(&Command::Xit(true)).is_none());
    }
}
