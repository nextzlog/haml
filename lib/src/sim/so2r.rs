use std::sync::{Arc, Mutex};

use crate::so2r::{Rx, So2r, So2rCaps};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimCommand {
    Tx(u8),
    Rx(Rx),
    Blend(bool, u8),
}

pub struct SimSo2r {
    log: Arc<Mutex<Vec<SimCommand>>>,
    tx: u8,
    rx: Rx,
    blend: Option<u8>,
}

impl SimSo2r {
    fn push(&self, c: SimCommand) {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(c);
    }
}

impl SimSo2r {
    pub fn new(log: Arc<Mutex<Vec<SimCommand>>>) -> Self {
        SimSo2r {
            log,
            tx: 1,
            rx: Rx::Rig1,
            blend: None,
        }
    }

    pub fn tx_radio(&self) -> u8 {
        self.tx
    }

    pub fn rx_audio(&self) -> Rx {
        self.rx
    }

    pub fn blending(&self) -> Option<u8> {
        self.blend
    }
}

impl So2r for SimSo2r {
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
        self.push(SimCommand::Tx(rig));
    }
    fn rx(&mut self, rx: Rx) {
        self.rx = rx;
        self.push(SimCommand::Rx(rx));
    }
    fn blend(&mut self, on: bool, percent: u8) {
        self.blend = on.then_some(percent);
        self.push(SimCommand::Blend(on, percent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simulated_box_records_what_it_is_told() {
        let log = Arc::default();
        let mut b = SimSo2r::new(Arc::clone(&log));
        b.select_rig(2);
        b.rx(Rx::Both);
        b.blend(true, 50);
        assert_eq!(
            (b.tx_radio(), b.rx_audio(), b.blending()),
            (2, Rx::Both, Some(50))
        );
        assert!(b.caps().auto_rx && b.error().is_none());
        assert_eq!(
            *log.lock().unwrap(),
            [
                SimCommand::Tx(2),
                SimCommand::Rx(Rx::Rig2),
                SimCommand::Rx(Rx::Both),
                SimCommand::Blend(true, 50)
            ]
        );
    }
}
