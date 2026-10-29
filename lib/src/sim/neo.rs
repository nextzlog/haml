use crate::so2r::Rx;
use crate::transport::{Emulated, Emulator, Transport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub tx: u8,
    pub rx: Rx,
    pub blend: bool,
    pub ratio: u8,
    pub received: usize,
    pub unknown: Vec<u8>,
}

impl Default for State {
    fn default() -> Self {
        State {
            tx: 1,
            rx: Rx::Rig1,
            blend: false,
            ratio: 0,
            received: 0,
            unknown: Vec::new(),
        }
    }
}

impl State {
    pub fn percent(&self) -> u8 {
        ((u32::from(self.ratio) * 100 + 127) / 255) as u8
    }
}

#[derive(Default)]
struct Inner {
    state: State,
    high: Option<u8>,
}

impl Inner {
    fn byte(&mut self, b: u8) {
        self.state.received += 1;
        match b {
            0x90..=0x92 | 0x94..=0x96 => {
                self.state.tx = if b & 0x04 != 0 { 2 } else { 1 };
                self.state.rx = match b & 0x03 {
                    0 => Rx::Rig1,
                    1 => Rx::Rig2,
                    _ => Rx::Both,
                };
            }
            0x86 | 0x87 => self.state.blend = b == 0x87,
            0xC0..=0xCF => match self.high.take() {
                None => self.high = Some(b & 0x0F),
                Some(h) => self.state.ratio = h << 4 | (b & 0x0F),
            },
            _ => self.state.unknown.push(b),
        }
    }
}

impl Emulator for Inner {
    fn receive(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.byte(*b);
        }
    }

    fn send(&mut self, _out: &mut Vec<u8>) {}
}

#[derive(Clone)]
pub struct NeoSim(Emulated<Inner>);

impl Default for NeoSim {
    fn default() -> Self {
        NeoSim(Emulated::new(Inner::default()))
    }
}

impl NeoSim {
    pub fn new() -> Self {
        NeoSim::default()
    }

    pub fn transport(&self) -> impl Transport + Send + use<> {
        self.0.clone()
    }

    pub fn state(&self) -> State {
        self.0.with(|g| g.state.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::so2r::neo::{blend_mode, blend_ratio, switch_rig};

    #[test]
    fn every_command_is_read_back() {
        let sim = NeoSim::new();
        let mut p = sim.transport();
        for tx in [1, 2] {
            for rx in [Rx::Rig1, Rx::Rig2, Rx::Both] {
                p.write(&[switch_rig(tx, rx)]).unwrap();
                assert_eq!((sim.state().tx, sim.state().rx), (tx, rx));
            }
        }
        p.write(&[blend_mode(true)]).unwrap();
        for percent in [0, 30, 50, 100] {
            p.write(&blend_ratio(percent)).unwrap();
            assert_eq!(sim.state().percent(), percent);
        }
        p.write(&[blend_mode(false), 0x93, 0x01]).unwrap();
        let s = sim.state();
        assert!(!s.blend);
        assert_eq!(s.unknown, [0x93, 0x01]);
        assert_eq!(s.received, 18);
    }
}
