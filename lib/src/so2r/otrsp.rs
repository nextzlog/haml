use crate::so2r::{Rx, So2r, So2rCaps};
use crate::transport::Boxed;

pub const BAUD: u32 = 9600;

pub fn tx(rig: u8) -> Vec<u8> {
    format!("TX{rig}\r").into_bytes()
}

pub fn rx(rig: u8) -> Vec<u8> {
    format!("RX{rig}\r").into_bytes()
}

pub struct Otrsp {
    port: Boxed,
    sent_tx: Option<u8>,
    sent_rx: Option<u8>,
    error: Option<String>,
}

impl Otrsp {
    pub fn new(port: Boxed) -> Self {
        Otrsp {
            port,
            sent_tx: None,
            sent_rx: None,
            error: None,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.error = self.port.write(bytes).err().map(|e| e.to_string());
    }
}

impl So2r for Otrsp {
    fn caps(&self) -> So2rCaps {
        So2rCaps {
            rx: true,
            ..So2rCaps::default()
        }
    }

    fn tx(&mut self, rig: u8) {
        self.send(&tx(rig));
        self.sent_tx = Some(rig);
    }

    fn rx(&mut self, rx: Rx) {
        let rig = match rx {
            Rx::Rig1 => 1,
            Rx::Rig2 => 2,
            Rx::Both => return,
        };
        self.send(&self::rx(rig));
        self.sent_rx = Some(rig);
    }

    fn select_rig(&mut self, rig: u8) {
        if self.sent_tx != Some(rig) {
            self.tx(rig);
        }
        if self.sent_rx != Some(rig) {
            self.rx(Rx::of(rig));
        }
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
    fn otrsp_sends_only_changes() {
        let (port, w) = Loop::pair();
        let mut s = Switch::new(Box::new(Otrsp::new(Box::new(port))));
        s.select_rig(1);
        assert_eq!(w.read_written(), b"TX1\rRX1\r");
        s.select_rig(1);
        assert!(w.read_written().is_empty());
        s.select_rig(2);
        assert_eq!(w.read_written(), b"TX2\rRX2\r");
        s.listen(Rx::Rig1);
        s.listen(Rx::Rig1);
        s.listen(Rx::Both);
        s.blend(true, 50);
        assert_eq!(w.read_written(), b"RX1\rRX1\r");
        assert!(!s.so2r.caps().auto_rx);
    }

    #[test]
    fn lines_of_the_box() {
        assert_eq!(tx(2), b"TX2\r");
        assert_eq!(rx(1), b"RX1\r");
    }
}
