use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::ports::Link;

pub trait Transport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize>;

    fn maintain(&mut self, _now: Instant) -> Link {
        Link::Connected
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        (**self).write(bytes)
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        (**self).read(buf)
    }

    fn maintain(&mut self, now: Instant) -> Link {
        (**self).maintain(now)
    }
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        (**self).write(bytes)
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        (**self).read(buf)
    }

    fn maintain(&mut self, now: Instant) -> Link {
        (**self).maintain(now)
    }
}

pub struct TcpTransport(TcpStream);

impl TcpTransport {
    pub fn connect(addr: impl std::net::ToSocketAddrs) -> io::Result<Self> {
        Self::new(TcpStream::connect(addr)?)
    }

    pub fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(TcpTransport(stream))
    }
}

impl Transport for TcpTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            match self.0.write(rest) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::yield_now(),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut chunk = [0; 1024];
        let mut total = 0;
        loop {
            match self.0.read(&mut chunk) {
                Ok(0) if total == 0 => return Err(ErrorKind::UnexpectedEof.into()),
                Ok(0) => return Ok(total),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    total += n;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(total),
                Err(e) => return Err(e),
            }
        }
    }
}

#[derive(Default)]
struct Queues {
    written: Vec<u8>,
    replies: VecDeque<u8>,
}

pub struct Loop(Arc<Mutex<Queues>>);

#[derive(Clone)]
pub struct Peer(Arc<Mutex<Queues>>);

impl Loop {
    pub fn pair() -> (Loop, Peer) {
        let q = Arc::default();
        (Loop(Arc::clone(&q)), Peer(q))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Peer {
    pub fn read_written(&self) -> Vec<u8> {
        std::mem::take(&mut lock(&self.0).written)
    }

    pub fn reply(&self, bytes: &[u8]) {
        lock(&self.0).replies.extend(bytes);
    }
}

impl Transport for Loop {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        lock(&self.0).written.extend_from_slice(bytes);
        Ok(())
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut g = lock(&self.0);
        let n = g.replies.len();
        buf.extend(g.replies.drain(..));
        Ok(n)
    }
}

pub trait Emulator: Send {
    fn receive(&mut self, bytes: &[u8]);
    fn send(&mut self, out: &mut Vec<u8>);
}

pub struct Emulated<E>(pub Arc<Mutex<E>>);

impl<E> Clone for Emulated<E> {
    fn clone(&self) -> Self {
        Emulated(Arc::clone(&self.0))
    }
}

impl<E: Emulator> Emulated<E> {
    pub fn new(emulator: E) -> Self {
        Emulated(Arc::new(Mutex::new(emulator)))
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut E) -> R) -> R {
        f(&mut lock(&self.0))
    }
}

impl<E: Emulator> Transport for Emulated<E> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.with(|e| e.receive(bytes));
        Ok(())
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let start = buf.len();
        self.with(|e| e.send(buf));
        Ok(buf.len() - start)
    }
}

pub type Boxed = Box<dyn Transport + Send>;

#[derive(Clone)]
pub struct Shared(Arc<Mutex<Boxed>>);

impl Shared {
    pub fn new(transport: Boxed) -> Self {
        Shared(Arc::new(Mutex::new(transport)))
    }

    pub fn same(&self, other: &Shared) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Transport for Shared {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        lock(&self.0).write(bytes)
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        lock(&self.0).read(buf)
    }

    fn maintain(&mut self, now: Instant) -> Link {
        lock(&self.0).maintain(now)
    }
}

#[derive(Clone, Default)]
pub struct Slot(Arc<Mutex<Option<Shared>>>);

impl Slot {
    pub fn publish(&self, port: Shared) {
        *lock(&self.0) = Some(port);
    }

    pub fn withdraw(&self) {
        *lock(&self.0) = None;
    }

    pub fn get(&self) -> Option<Shared> {
        lock(&self.0).clone()
    }
}

#[cfg(feature = "serial")]
pub use serial::SerialTransport;

#[cfg(feature = "serial")]
mod serial {
    use super::Transport;
    use crate::ptt::{Line, Lines};
    use std::io::{self, Read, Write};
    use std::time::Duration;

    pub struct SerialTransport(Box<dyn serialport::SerialPort>);

    impl SerialTransport {
        pub fn open(path: &str, baud: u32, stop_bits: u8) -> io::Result<Self> {
            let stop = if stop_bits == 2 {
                serialport::StopBits::Two
            } else {
                serialport::StopBits::One
            };
            let port = serialport::new(path, baud)
                .stop_bits(stop)
                .data_bits(serialport::DataBits::Eight)
                .timeout(Duration::from_millis(100))
                .open()
                .map_err(io::Error::other)?;
            Ok(SerialTransport(port))
        }

        pub fn from_port(port: Box<dyn serialport::SerialPort>) -> Self {
            SerialTransport(port)
        }

        pub fn port(&mut self) -> &mut dyn serialport::SerialPort {
            self.0.as_mut()
        }
    }

    impl Transport for SerialTransport {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.0.write_all(bytes)
        }

        fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
            let n = self.0.bytes_to_read()? as usize;
            if n == 0 {
                return Ok(0);
            }
            let start = buf.len();
            buf.resize(start + n, 0);
            let got = self.0.read(&mut buf[start..])?;
            buf.truncate(start + got);
            Ok(got)
        }
    }

    impl Lines for SerialTransport {
        fn set(&mut self, line: Line, on: bool) -> io::Result<()> {
            match line {
                Line::Dtr => self.0.write_data_terminal_ready(on),
                Line::Rts => self.0.write_request_to_send(on),
            }
            .map_err(io::Error::other)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_carries_bytes_both_ways_and_reads_what_is_there() {
        let (mut port, peer) = Loop::pair();
        port.write(b"FA;").unwrap();
        port.write(b"MD;").unwrap();
        assert_eq!(peer.read_written(), b"FA;MD;");
        assert!(peer.read_written().is_empty());
        let mut buf = Vec::new();
        assert_eq!(port.read(&mut buf).unwrap(), 0);
        peer.reply(b"FA0");
        port.read(&mut buf).unwrap();
        peer.reply(b"7012000;");
        assert_eq!(port.read(&mut buf).unwrap(), 8);
        assert_eq!(buf, b"FA07012000;");
    }

    #[derive(Default)]
    struct Upper(Vec<u8>);

    impl Emulator for Upper {
        fn receive(&mut self, bytes: &[u8]) {
            self.0.extend(bytes.to_ascii_uppercase());
        }
        fn send(&mut self, out: &mut Vec<u8>) {
            out.append(&mut self.0);
        }
    }

    #[test]
    fn an_emulator_answers_through_the_transport() {
        let e = Emulated::new(Upper::default());
        let mut t: Boxed = Box::new(e.clone());
        t.write(b"cq").unwrap();
        assert_eq!(e.with(|u| u.0.clone()), b"CQ");
        let mut buf = Vec::new();
        assert_eq!(t.read(&mut buf).unwrap(), 2);
        assert_eq!(buf, b"CQ");
    }

    #[test]
    fn a_slot_shares_a_port_while_published() {
        let (port, peer) = Loop::pair();
        let slot = Slot::default();
        assert!(slot.get().is_none());
        let shared = Shared::new(Box::new(port));
        slot.publish(shared.clone());
        let mut other = slot.get().unwrap();
        assert!(other.same(&shared));
        other.write(&[0x94]).unwrap();
        assert_eq!(peer.read_written(), [0x94]);
        slot.withdraw();
        assert!(slot.get().is_none());
    }
}
