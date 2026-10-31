#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rx {
    Rig1,
    Rig2,
    Both,
}

impl Rx {
    pub fn of(radio: u8) -> Rx {
        if radio == 2 {
            Rx::Rig2
        } else {
            Rx::Rig1
        }
    }

    pub fn other(tx: u8) -> Rx {
        if tx == 2 {
            Rx::Rig1
        } else {
            Rx::Rig2
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct So2rCaps {
    pub rx: bool,
    pub rx_both: bool,
    pub blend: bool,
    pub auto_rx: bool,
}

pub trait So2r: Send {
    fn caps(&self) -> So2rCaps;

    fn tx(&mut self, radio: u8);

    fn rx(&mut self, rx: Rx);

    fn select_rig(&mut self, radio: u8) {
        self.tx(radio);
        self.rx(Rx::of(radio));
    }

    fn blend(&mut self, _on: bool, _percent: u8) {}

    fn poll(&mut self) {}

    fn error(&self) -> Option<String> {
        None
    }
}

impl<S: So2r + ?Sized> So2r for Box<S> {
    fn caps(&self) -> So2rCaps {
        (**self).caps()
    }
    fn tx(&mut self, radio: u8) {
        (**self).tx(radio)
    }
    fn rx(&mut self, rx: Rx) {
        (**self).rx(rx)
    }
    fn select_rig(&mut self, radio: u8) {
        (**self).select_rig(radio)
    }
    fn blend(&mut self, on: bool, percent: u8) {
        (**self).blend(on, percent)
    }
    fn poll(&mut self) {
        (**self).poll()
    }
    fn error(&self) -> Option<String> {
        (**self).error()
    }
}

pub struct Switch<S> {
    pub so2r: S,
    pub tx: u8,
    pub rx: Rx,
}

impl<S: So2r> Switch<S> {
    pub fn new(so2r: S) -> Self {
        Switch {
            so2r,
            tx: 1,
            rx: Rx::Rig1,
        }
    }

    pub fn select_rig(&mut self, radio: u8) {
        self.tx = radio;
        self.rx = Rx::of(radio);
        self.so2r.select_rig(radio);
    }

    pub fn transmit_on(&mut self, radio: u8) {
        if self.tx != radio {
            self.tx = radio;
            self.so2r.tx(radio);
        }
    }

    pub fn listen(&mut self, rx: Rx) {
        self.rx = rx;
        self.so2r.rx(rx);
    }

    pub fn blend(&mut self, on: bool, percent: u8) {
        self.so2r.blend(on, percent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<String>);

    impl So2r for Log {
        fn caps(&self) -> So2rCaps {
            So2rCaps::default()
        }
        fn tx(&mut self, radio: u8) {
            self.0.push(format!("TX{radio}"));
        }
        fn rx(&mut self, rx: Rx) {
            self.0.push(format!("{rx:?}"));
        }
    }

    #[test]
    fn transmit_on_tells_the_box_only_of_a_change() {
        let mut s = Switch::new(Log::default());
        s.transmit_on(1);
        s.transmit_on(2);
        s.transmit_on(2);
        assert_eq!((s.tx, s.rx), (2, Rx::Rig1));
        assert_eq!(s.so2r.0, ["TX2"]);
        s.select_rig(1);
        assert_eq!(s.so2r.0, ["TX2", "TX1", "Rig1"]);
    }

    #[test]
    fn receivers() {
        assert_eq!((Rx::of(1), Rx::of(2)), (Rx::Rig1, Rx::Rig2));
        assert_eq!((Rx::other(1), Rx::other(2)), (Rx::Rig2, Rx::Rig1));
    }
}

pub mod neo;
pub use ops::{Operator, RxMode};
pub mod ops;
pub mod otrsp;
