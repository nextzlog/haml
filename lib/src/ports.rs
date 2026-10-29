use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::transport::Transport;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsbId {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub name: String,
    pub usb: Option<UsbId>,
    pub description: String,
}

impl PortInfo {
    pub fn named(name: &str) -> Self {
        PortInfo {
            name: name.into(),
            usb: None,
            description: String::new(),
        }
    }

    pub fn detail(&self) -> String {
        match &self.usb {
            Some(u) if self.description.is_empty() => format!("USB {:04X}:{:04X}", u.vid, u.pid),
            Some(u) => format!("USB {:04X}:{:04X} {}", u.vid, u.pid, self.description),
            None => self.description.clone(),
        }
    }
}

pub type Scan = Result<Vec<PortInfo>, String>;

pub trait Scanner: Send + 'static {
    fn scan(&mut self) -> Scan;
}

impl<F: FnMut() -> Scan + Send + 'static> Scanner for F {
    fn scan(&mut self) -> Scan {
        self()
    }
}

#[cfg(feature = "serial")]
pub fn serial_ports() -> Scan {
    use serialport::SerialPortType;
    let ports = serialport::available_ports().map_err(|e| e.to_string())?;
    Ok(ports
        .into_iter()
        .map(|p| match p.port_type {
            SerialPortType::UsbPort(u) => PortInfo {
                name: p.port_name,
                description: [u.manufacturer.as_deref(), u.product.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" "),
                usb: Some(UsbId {
                    vid: u.vid,
                    pid: u.pid,
                    serial: u.serial_number,
                }),
            },
            SerialPortType::BluetoothPort => PortInfo {
                description: "Bluetooth".into(),
                ..PortInfo::named(&p.port_name)
            },
            SerialPortType::PciPort => PortInfo {
                description: "PCI".into(),
                ..PortInfo::named(&p.port_name)
            },
            SerialPortType::Unknown => PortInfo::named(&p.port_name),
        })
        .collect())
}

pub struct Watcher {
    events: Receiver<Scan>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    pub fn spawn(mut scanner: impl Scanner, interval: Duration) -> Self {
        let (tx, events) = mpsc::channel();
        let (stop, halt) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("haml-ports".into())
            .spawn(move || {
                let mut last: Option<Scan> = None;
                loop {
                    let now = scanner.scan();
                    if last.as_ref() != Some(&now) {
                        last = Some(now.clone());
                        if tx.send(now).is_err() {
                            return;
                        }
                    }
                    if halt.recv_timeout(interval) != Err(RecvTimeoutError::Timeout) {
                        return;
                    }
                }
            })
            .expect("port watcher thread");
        Watcher {
            events,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    pub fn changes(&self) -> Vec<Scan> {
        self.events.try_iter().collect()
    }

    pub fn events(&self) -> &Receiver<Scan> {
        &self.events
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Name(String),
    Usb(UsbId),
}

impl Target {
    pub fn of(port: &PortInfo) -> Target {
        match &port.usb {
            Some(id) => Target::Usb(id.clone()),
            None => Target::Name(port.name.clone()),
        }
    }

    pub fn resolve<'a>(&self, ports: &'a [PortInfo]) -> Option<&'a PortInfo> {
        ports.iter().find(|p| match (self, &p.usb) {
            (Target::Name(n), _) => p.name == *n,
            (Target::Usb(want), Some(got)) => {
                want.vid == got.vid
                    && want.pid == got.pid
                    && (want.serial.is_none() || want.serial == got.serial)
            }
            (Target::Usb(_), None) => false,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connected,
    Disconnected,
}

type Opener<T> = Box<dyn FnMut(&str) -> io::Result<T> + Send>;

pub struct Reconnecting<T> {
    target: Target,
    scanner: Box<dyn Scanner>,
    open: Opener<T>,
    port: Option<T>,
    retry: Duration,
    next: Option<Instant>,
    error: Option<String>,
}

impl<T: Transport> Reconnecting<T> {
    pub fn new(
        target: Target,
        scanner: impl Scanner,
        open: impl FnMut(&str) -> io::Result<T> + Send + 'static,
        retry: Duration,
    ) -> Self {
        Reconnecting {
            target,
            scanner: Box::new(scanner),
            open: Box::new(open),
            port: None,
            retry,
            next: None,
            error: None,
        }
    }

    pub fn link(&self) -> Link {
        if self.port.is_some() {
            Link::Connected
        } else {
            Link::Disconnected
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn port(&mut self) -> Option<&mut T> {
        self.port.as_mut()
    }

    fn connect(&mut self) -> Result<T, String> {
        let ports = self.scanner.scan()?;
        let found = self
            .target
            .resolve(&ports)
            .ok_or_else(|| "the port is not plugged in".to_string())?;
        (self.open)(&found.name).map_err(|e| e.to_string())
    }

    fn lost<R>(&mut self, e: io::Error) -> io::Result<R> {
        self.port = None;
        self.next = None;
        self.error = Some(e.to_string());
        Err(e)
    }
}

impl<T: Transport> Transport for Reconnecting<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(p) = self.port.as_mut() else {
            return Err(io::ErrorKind::NotConnected.into());
        };
        match p.write(bytes) {
            Ok(()) => Ok(()),
            Err(e) => self.lost(e),
        }
    }

    fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let Some(p) = self.port.as_mut() else {
            return Err(io::ErrorKind::NotConnected.into());
        };
        match p.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => self.lost(e),
        }
    }

    fn maintain(&mut self, now: Instant) -> Link {
        if self.port.is_none() && self.next.is_none_or(|t| now >= t) {
            match self.connect() {
                Ok(p) => {
                    self.port = Some(p);
                    self.error = None;
                }
                Err(e) => {
                    self.error = Some(e);
                    self.next = Some(now + self.retry);
                }
            }
        }
        self.link()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Loop, Peer};
    use std::sync::{Arc, Mutex};

    fn usb(name: &str, serial: &str) -> PortInfo {
        PortInfo {
            name: name.into(),
            usb: Some(UsbId {
                vid: 0x0403,
                pid: 0x6001,
                serial: Some(serial.into()),
            }),
            description: "FTDI".into(),
        }
    }

    #[test]
    fn a_usb_port_is_found_again_under_another_name() {
        let target = Target::of(&usb("COM3", "A1"));
        let later = [usb("COM7", "B2"), usb("COM9", "A1")];
        assert_eq!(target.resolve(&later).unwrap().name, "COM9");
        assert!(target.resolve(&[PortInfo::named("COM3")]).is_none());
        let any = Target::Usb(UsbId {
            serial: None,
            ..later[0].usb.clone().unwrap()
        });
        assert_eq!(any.resolve(&later).unwrap().name, "COM7");
        assert_eq!(
            Target::Name("COM3".into())
                .resolve(&[PortInfo::named("COM3")])
                .unwrap()
                .name,
            "COM3"
        );
        assert_eq!(later[0].detail(), "USB 0403:6001 FTDI");
    }

    fn settle(w: &Watcher, want: &[&str]) {
        let start = Instant::now();
        loop {
            if let Ok(Ok(ports)) = w.events().recv_timeout(Duration::from_secs(5)) {
                let names: Vec<_> = ports.iter().map(|p| p.name.as_str()).collect();
                if names == want {
                    return;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "never saw {want:?}"
            );
        }
    }

    #[test]
    fn the_watcher_reports_only_changes_and_never_scans_on_the_caller() {
        let found = Arc::new(Mutex::new(vec![PortInfo::named("COM1")]));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let (f, t) = (found.clone(), threads.clone());
        let w = Watcher::spawn(
            move || {
                t.lock().unwrap().push(std::thread::current().id());
                Ok(f.lock().unwrap().clone())
            },
            Duration::from_millis(2),
        );
        settle(&w, &["COM1"]);
        std::thread::sleep(Duration::from_millis(30));
        assert!(w.changes().is_empty(), "same ports");
        found.lock().unwrap().push(PortInfo::named("COM4"));
        settle(&w, &["COM1", "COM4"]);
        drop(w);
        let me = std::thread::current().id();
        assert!(threads.lock().unwrap().iter().all(|t| *t != me));
    }

    #[test]
    fn a_lost_port_is_a_state_and_comes_back() {
        let plugged = Arc::new(Mutex::new(vec![usb("COM3", "A1")]));
        let peers: Arc<Mutex<Vec<Peer>>> = Arc::default();
        let (p, opened) = (plugged.clone(), peers.clone());
        let mut r = Reconnecting::new(
            Target::of(&usb("COM3", "A1")),
            move || Ok(p.lock().unwrap().clone()),
            move |name: &str| {
                assert_eq!(name, "COM5");
                let (port, peer) = Loop::pair();
                opened.lock().unwrap().push(peer);
                Ok(port)
            },
            Duration::from_millis(100),
        );
        let t0 = Instant::now();
        *plugged.lock().unwrap() = vec![];
        assert_eq!(r.maintain(t0), Link::Disconnected);
        assert!(r.write(b"x").is_err());
        assert_eq!(r.error(), Some("the port is not plugged in"));
        *plugged.lock().unwrap() = vec![usb("COM5", "A1")];
        assert_eq!(
            r.maintain(t0 + Duration::from_millis(50)),
            Link::Disconnected
        );
        assert_eq!(r.maintain(t0 + Duration::from_millis(100)), Link::Connected);
        r.write(b"FA;").unwrap();
        assert_eq!(peers.lock().unwrap()[0].read_written(), b"FA;");
    }
}
