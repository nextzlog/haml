use crate::so2r::{Rx, So2r, So2rCaps};
use crate::transport::{Boxed, Shared, Slot, Transport};

pub const NO_KEYER: &str = "no WinKeyer is open to carry the bytes";

pub fn switch_rig(tx: u8, rx: Rx) -> u8 {
    let tx = if tx == 2 { 0x94 } else { 0x90 };
    tx + rx as u8
}

pub fn blend_mode(on: bool) -> u8 {
    if on {
        0x87
    } else {
        0x86
    }
}

pub fn blend_ratio(percent: u8) -> [u8; 2] {
    let r = (255 * u32::from(percent.min(100)) / 100) as u8;
    [0xC0 | (r >> 4), 0xC0 | (r & 0x0F)]
}

enum Out {
    Own(Boxed),
    Keyer(Slot, Option<Shared>),
}

pub struct Neo {
    out: Out,
    tx: u8,
    rx: Rx,
    blend: Option<(bool, u8)>,
    told: bool,
    error: Option<String>,
}

impl Neo {
    fn new(out: Out) -> Self {
        Neo {
            out,
            tx: 1,
            rx: Rx::Rig1,
            blend: None,
            told: false,
            error: None,
        }
    }

    pub fn on_port(port: Boxed) -> Self {
        Neo::new(Out::Own(port))
    }

    pub fn on_keyer(slot: Slot) -> Self {
        Neo::new(Out::Keyer(slot, None))
    }

    fn state_bytes(&self) -> Vec<u8> {
        let mut b = vec![switch_rig(self.tx, self.rx)];
        if let Some((on, ratio)) = self.blend {
            b.push(blend_mode(on));
            if on {
                b.extend(blend_ratio(ratio));
            }
        }
        b
    }

    fn send(&mut self, bytes: &[u8]) {
        self.told = true;
        let state = self.state_bytes();
        let r = match &mut self.out {
            Out::Own(p) => p.write(bytes).map_err(|e| e.to_string()),
            Out::Keyer(slot, last) => match slot.get() {
                None => {
                    *last = None;
                    Err(NO_KEYER.to_string())
                }
                Some(mut port) => {
                    let again = last.as_ref().is_some_and(|l| l.same(&port));
                    let r = if again {
                        port.write(bytes)
                    } else {
                        port.write(&state)
                    };
                    *last = Some(port);
                    r.map_err(|e| e.to_string())
                }
            },
        };
        self.error = r.err();
    }
}

impl So2r for Neo {
    fn caps(&self) -> So2rCaps {
        So2rCaps {
            rx: true,
            rx_both: true,
            blend: true,
            auto_rx: true,
        }
    }

    fn tx(&mut self, rig: u8) {
        self.tx = rig;
        self.send(&[switch_rig(rig, self.rx)]);
    }

    fn rx(&mut self, rx: Rx) {
        self.rx = rx;
        self.send(&[switch_rig(self.tx, rx)]);
    }

    fn blend(&mut self, on: bool, percent: u8) {
        let ratio = self
            .blend
            .map_or(percent, |(_, r)| if on { percent } else { r });
        self.blend = Some((on, ratio));
        let mut b = vec![blend_mode(on)];
        if on {
            b.extend(blend_ratio(percent));
        }
        self.send(&b);
    }

    fn poll(&mut self) {
        let state = self.state_bytes();
        let Out::Keyer(slot, last) = &mut self.out else {
            return;
        };
        let Some(mut port) = slot.get() else {
            *last = None;
            return;
        };
        if !self.told || last.as_ref().is_some_and(|l| l.same(&port)) {
            return;
        }
        let r = port.write(&state).map_err(|e| e.to_string());
        *last = Some(port);
        self.error = r.err();
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::so2r::Switch;
    use crate::transport::Loop;

    #[test]
    fn neo_switches_tx_then_rx() {
        let (port, w) = Loop::pair();
        let mut s = Switch::new(Box::new(Neo::on_port(Box::new(port))));
        s.select_rig(2);
        assert_eq!(w.read_written(), [0x94, 0x95]);
        s.listen(Rx::Both);
        assert_eq!(w.read_written(), [0x96]);
        s.select_rig(1);
        assert_eq!(w.read_written(), [0x92, 0x90]);
        s.blend(true, 50);
        s.blend(false, 50);
        assert_eq!(w.read_written(), [0x87, 0xC7, 0xCF, 0x86]);
        assert!(s.so2r.caps().auto_rx && s.so2r.error().is_none());
    }

    #[test]
    fn on_the_keyer_port_the_bytes_reach_the_keyers_wire() {
        let hub = Slot::default();
        let mut b = Neo::on_keyer(hub.clone());
        b.select_rig(2);
        b.blend(true, 50);
        assert!(b.error().is_some(), "no keyer yet");
        b.poll();
        for switch in [0x95, 0x96] {
            let (port, w) = Loop::pair();
            hub.publish(Shared::new(Box::new(port)));
            b.poll();
            assert_eq!(w.read_written(), [switch, 0x87, 0xC7, 0xCF]);
            b.poll();
            assert!(w.read_written().is_empty());
            b.rx(Rx::Both);
            assert_eq!(w.read_written(), [0x96]);
            assert!(b.error().is_none());
            hub.withdraw();
        }
        let (port, w) = Loop::pair();
        hub.publish(Shared::new(Box::new(port)));
        b.rx(Rx::Rig1);
        assert_eq!(w.read_written(), [0x94, 0x87, 0xC7, 0xCF]);
    }

    #[test]
    fn nothing_before_the_first_command() {
        let hub = Slot::default();
        let (port, w) = Loop::pair();
        hub.publish(Shared::new(Box::new(port)));
        let mut b = Neo::on_keyer(hub.clone());
        b.poll();
        assert!(w.read_written().is_empty());
    }

    #[test]
    fn neo_tx_toggle_keeps_the_receiver() {
        let (port, w) = Loop::pair();
        let mut s = Switch::new(Box::new(Neo::on_port(Box::new(port))));
        s.select_rig(1);
        w.read_written();
        s.transmit_on(2);
        assert_eq!(w.read_written(), [0x94]);
        s.listen(Rx::Both);
        s.transmit_on(1);
        assert_eq!(w.read_written(), [0x96, 0x92]);
    }

    mod bytes {
        use super::*;

        #[test]
        fn bytes_of_the_box() {
            assert_eq!(switch_rig(1, Rx::Rig1), 0x90);
            assert_eq!(switch_rig(1, Rx::Rig2), 0x91);
            assert_eq!(switch_rig(1, Rx::Both), 0x92);
            assert_eq!(switch_rig(2, Rx::Rig1), 0x94);
            assert_eq!(switch_rig(2, Rx::Rig2), 0x95);
            assert_eq!(switch_rig(2, Rx::Both), 0x96);
            assert_eq!((blend_mode(true), blend_mode(false)), (0x87, 0x86));
            assert_eq!(blend_ratio(50), [0xC7, 0xCF]);
            assert_eq!(blend_ratio(100), [0xCF, 0xCF]);
            assert_eq!(blend_ratio(0), [0xC0, 0xC0]);
        }
    }
}
