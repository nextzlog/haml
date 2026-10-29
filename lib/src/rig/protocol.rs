use crate::{Command, Event, RigCaps};

pub trait Protocol: Send {
    fn encode(&mut self, command: &Command) -> Option<Vec<u8>>;

    fn poll(&mut self) -> Vec<u8>;

    fn decode(&mut self, bytes: &[u8]) -> Vec<Event>;

    fn acknowledges(&self) -> bool;

    fn caps(&self) -> RigCaps {
        RigCaps::ALL
    }

    fn verify(&mut self, _command: &Command) -> Option<Vec<u8>> {
        None
    }

    fn reset(&mut self) {}
}

impl<P: Protocol + ?Sized> Protocol for Box<P> {
    fn encode(&mut self, command: &Command) -> Option<Vec<u8>> {
        (**self).encode(command)
    }

    fn poll(&mut self) -> Vec<u8> {
        (**self).poll()
    }

    fn decode(&mut self, bytes: &[u8]) -> Vec<Event> {
        (**self).decode(bytes)
    }

    fn acknowledges(&self) -> bool {
        (**self).acknowledges()
    }

    fn caps(&self) -> RigCaps {
        (**self).caps()
    }

    fn verify(&mut self, command: &Command) -> Option<Vec<u8>> {
        (**self).verify(command)
    }

    fn reset(&mut self) {
        (**self).reset()
    }
}

pub(crate) const MAX_PENDING: usize = 256;
