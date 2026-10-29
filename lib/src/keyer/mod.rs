use crate::error::{Error, Result};
use crate::morse::Timing;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyerEvent {
    Finished,
    Speed(u32),
    Paddle(bool),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyerCaps {
    pub ptt: bool,
    pub ptt_delays: bool,
    pub paddle: bool,
    pub speed_pot: bool,
    pub fsk: bool,
}

pub trait Keyer: Send {
    fn caps(&self) -> KeyerCaps;

    fn send(&mut self, text: &str);

    fn abort(&mut self);

    fn set_wpm(&mut self, wpm: u32);

    fn wpm(&self) -> u32;

    fn set_timing(&mut self, timing: Timing) {
        self.set_wpm(timing.wpm);
    }

    fn set_ptt(&mut self, on: bool);

    fn set_phone_ptt(&mut self, on: bool) {
        self.set_ptt(on);
    }

    fn send_fsk(&mut self, _text: &str) -> Result<()> {
        Err(Error::Unsupported)
    }

    fn busy(&self) -> bool;

    fn poll(&mut self) -> Vec<KeyerEvent>;
}

impl<K: Keyer + ?Sized> Keyer for Box<K> {
    fn caps(&self) -> KeyerCaps {
        (**self).caps()
    }
    fn send(&mut self, text: &str) {
        (**self).send(text)
    }
    fn abort(&mut self) {
        (**self).abort()
    }
    fn set_wpm(&mut self, wpm: u32) {
        (**self).set_wpm(wpm)
    }
    fn wpm(&self) -> u32 {
        (**self).wpm()
    }
    fn set_timing(&mut self, timing: Timing) {
        (**self).set_timing(timing)
    }
    fn set_ptt(&mut self, on: bool) {
        (**self).set_ptt(on)
    }
    fn set_phone_ptt(&mut self, on: bool) {
        (**self).set_phone_ptt(on)
    }
    fn send_fsk(&mut self, text: &str) -> Result<()> {
        (**self).send_fsk(text)
    }
    fn busy(&self) -> bool {
        (**self).busy()
    }
    fn poll(&mut self) -> Vec<KeyerEvent> {
        (**self).poll()
    }
}

pub mod if4cw;
pub mod line;
pub mod winkeyer;
