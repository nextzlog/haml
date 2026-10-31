use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

const MAX_LINE: usize = 1 << 16;

pub trait Conn: Send {
    fn peer(&self) -> String;

    fn read_line(&mut self, wait: Duration) -> io::Result<Option<String>>;

    fn write_line(&mut self, line: &str) -> io::Result<()>;

    fn shutdown(&mut self);
}

pub trait Listener: Send {
    fn accept(&mut self, wait: Duration) -> io::Result<Option<Box<dyn Conn>>>;
}

impl Listener for Receiver<Box<dyn Conn>> {
    fn accept(&mut self, wait: Duration) -> io::Result<Option<Box<dyn Conn>>> {
        match self.recv_timeout(wait) {
            Ok(c) => Ok(Some(c)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}

pub struct PairConn {
    peer: String,
    rx: Receiver<String>,
    tx: Option<Sender<String>>,
}

pub fn pair(a: &str, b: &str) -> (PairConn, PairConn) {
    let (ta, rb) = mpsc::channel();
    let (tb, ra) = mpsc::channel();
    let end = |peer: &str, rx, tx| PairConn {
        peer: peer.into(),
        rx,
        tx: Some(tx),
    };
    (end(b, ra, ta), end(a, rb, tb))
}

impl Conn for PairConn {
    fn peer(&self) -> String {
        self.peer.clone()
    }

    fn read_line(&mut self, wait: Duration) -> io::Result<Option<String>> {
        if self.tx.is_none() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        match self.rx.recv_timeout(wait) {
            Ok(l) => Ok(Some(l)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.tx
            .as_ref()
            .and_then(|t| t.send(line.into()).ok())
            .ok_or_else(|| io::ErrorKind::BrokenPipe.into())
    }

    fn shutdown(&mut self) {
        self.tx = None;
    }
}

pub struct TcpConn {
    stream: TcpStream,
    peer: String,
    pending: Vec<u8>,
}

impl TcpConn {
    pub fn loopback(port: u16) -> io::Result<TcpConn> {
        TcpConn::new(TcpStream::connect(("127.0.0.1", port))?)
    }

    fn new(stream: TcpStream) -> io::Result<TcpConn> {
        stream.set_nodelay(true)?;
        Ok(TcpConn {
            peer: stream.peer_addr()?.to_string(),
            stream,
            pending: Vec::new(),
        })
    }

    fn take(&mut self) -> Option<String> {
        let i = self.pending.iter().position(|&b| b == b'\n')?;
        let line: Vec<u8> = self.pending.drain(..=i).collect();
        Some(
            String::from_utf8_lossy(&line)
                .trim_end_matches(['\r', '\n'])
                .to_string(),
        )
    }
}

impl Conn for TcpConn {
    fn peer(&self) -> String {
        self.peer.clone()
    }

    fn read_line(&mut self, wait: Duration) -> io::Result<Option<String>> {
        if let Some(line) = self.take() {
            return Ok(Some(line));
        }
        self.stream
            .set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
        let mut chunk = [0u8; 4096];
        match self.stream.read(&mut chunk) {
            Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                self.pending.extend_from_slice(&chunk[..n]);
                if self.pending.len() > MAX_LINE {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                Ok(self.take())
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.stream.write_all(format!("{line}\n").as_bytes())?;
        self.stream.flush()
    }

    fn shutdown(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

pub struct LoopbackListener(TcpListener);

impl LoopbackListener {
    pub fn bind(port: u16) -> io::Result<LoopbackListener> {
        let l = TcpListener::bind(("127.0.0.1", port))?;
        l.set_nonblocking(true)?;
        Ok(LoopbackListener(l))
    }

    pub fn port(&self) -> io::Result<u16> {
        Ok(self.0.local_addr()?.port())
    }
}

impl Listener for LoopbackListener {
    fn accept(&mut self, wait: Duration) -> io::Result<Option<Box<dyn Conn>>> {
        match self.0.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false)?;
                Ok(Some(Box::new(TcpConn::new(s)?)))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(wait.min(Duration::from_millis(20)));
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
