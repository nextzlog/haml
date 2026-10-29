use std::collections::HashMap;
use std::io;
use std::sync::mpsc::{channel, Sender};

use crate::ports::{PortInfo, Scan, Scanner};

pub trait Hid: Send {
    fn write(&mut self, report: &[u8]) -> io::Result<()>;
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

impl<H: Hid + ?Sized> Hid for Box<H> {
    fn write(&mut self, report: &[u8]) -> io::Result<()> {
        (**self).write(report)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read(buf)
    }
}

pub trait Backend: Send + 'static {
    fn enumerate(&mut self) -> Scan;
    fn open(&mut self, path: &str) -> Result<(Box<dyn Hid>, String), String>;
}

type Reply<T> = Sender<T>;

enum Req {
    Scan(Reply<Scan>),
    Open(String, Reply<Result<(u64, String), String>>),
    Write(u64, Vec<u8>, Reply<io::Result<()>>),
    Read(u64, usize, Reply<io::Result<Vec<u8>>>),
    Close(u64),
}

#[derive(Clone)]
pub struct HidHub {
    tx: Sender<Req>,
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the HID thread has stopped")
}

impl HidHub {
    pub fn spawn(mut backend: impl Backend) -> Self {
        let (tx, rx) = channel::<Req>();
        std::thread::Builder::new()
            .name("haml-hid".into())
            .spawn(move || {
                let mut devices: HashMap<u64, Box<dyn Hid>> = HashMap::new();
                let mut next = 0;
                for req in rx {
                    match req {
                        Req::Scan(reply) => {
                            let _ = reply.send(backend.enumerate());
                        }
                        Req::Open(path, reply) => {
                            let got = backend.open(&path).map(|(hid, product)| {
                                next += 1;
                                devices.insert(next, hid);
                                (next, product)
                            });
                            let _ = reply.send(got);
                        }
                        Req::Write(id, report, reply) => {
                            let got = devices.get_mut(&id).map_or_else(
                                || Err(io::ErrorKind::NotConnected.into()),
                                |d| d.write(&report),
                            );
                            let _ = reply.send(got);
                        }
                        Req::Read(id, len, reply) => {
                            let mut buf = vec![0; len];
                            let got = devices.get_mut(&id).map_or_else(
                                || Err(io::ErrorKind::NotConnected.into()),
                                |d| d.read(&mut buf),
                            );
                            let _ = reply.send(got.map(|n| {
                                buf.truncate(n);
                                buf
                            }));
                        }
                        Req::Close(id) => {
                            devices.remove(&id);
                        }
                    }
                }
            })
            .expect("HID thread");
        HidHub { tx }
    }

    #[cfg(feature = "hid")]
    pub fn native(vid: u16, pid: u16) -> Self {
        HidHub::spawn(native::Native::new(vid, pid))
    }

    pub fn open(&self, path: &str) -> Result<(Box<dyn Hid>, String), String> {
        let (reply, rx) = channel();
        self.tx
            .send(Req::Open(path.into(), reply))
            .map_err(|_| gone().to_string())?;
        let (id, product) = rx.recv().map_err(|_| gone().to_string())??;
        let remote = Remote {
            id,
            tx: self.tx.clone(),
        };
        Ok((Box::new(remote), product))
    }
}

impl Scanner for HidHub {
    fn scan(&mut self) -> Scan {
        let (reply, rx) = channel();
        self.tx
            .send(Req::Scan(reply))
            .map_err(|_| gone().to_string())?;
        rx.recv().map_err(|_| gone().to_string())?
    }
}

struct Remote {
    id: u64,
    tx: Sender<Req>,
}

impl Remote {
    fn call<T>(&self, req: impl FnOnce(Reply<io::Result<T>>) -> Req) -> io::Result<T> {
        let (reply, rx) = channel();
        self.tx.send(req(reply)).map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?
    }
}

impl Hid for Remote {
    fn write(&mut self, report: &[u8]) -> io::Result<()> {
        let id = self.id;
        self.call(|r| Req::Write(id, report.to_vec(), r))
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let id = self.id;
        let got = self.call(|r| Req::Read(id, buf.len(), r))?;
        buf[..got.len()].copy_from_slice(&got);
        Ok(got.len())
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.tx.send(Req::Close(self.id));
    }
}

pub fn listed(found: &[PortInfo]) -> Vec<String> {
    found.iter().map(|p| p.name.clone()).collect()
}

#[cfg(feature = "hid")]
mod native {
    use super::{Backend, Hid};
    use crate::ports::{PortInfo, Scan, UsbId};
    use std::io;

    pub struct Native {
        api: Option<hidapi::HidApi>,
        vid: u16,
        pid: u16,
    }

    impl Native {
        pub fn new(vid: u16, pid: u16) -> Self {
            Native {
                api: None,
                vid,
                pid,
            }
        }

        fn api(&mut self) -> Result<&mut hidapi::HidApi, String> {
            match &mut self.api {
                Some(api) => api.refresh_devices().map_err(|e| e.to_string())?,
                None => self.api = Some(hidapi::HidApi::new().map_err(|e| e.to_string())?),
            }
            Ok(self.api.as_mut().expect("set above"))
        }
    }

    struct Device(hidapi::HidDevice);

    impl Hid for Device {
        fn write(&mut self, report: &[u8]) -> io::Result<()> {
            self.0.write(report).map(drop).map_err(io::Error::other)
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read_timeout(buf, 0).map_err(io::Error::other)
        }
    }

    impl Backend for Native {
        fn enumerate(&mut self) -> Scan {
            let (vid, pid) = (self.vid, self.pid);
            let api = self.api()?;
            Ok(api
                .device_list()
                .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
                .map(|d| PortInfo {
                    name: d.path().to_string_lossy().into_owned(),
                    usb: Some(UsbId {
                        vid,
                        pid,
                        serial: d.serial_number().map(str::to_string),
                    }),
                    description: d.product_string().unwrap_or_default().to_string(),
                })
                .collect())
        }

        fn open(&mut self, path: &str) -> Result<(Box<dyn Hid>, String), String> {
            let (vid, pid) = (self.vid, self.pid);
            let api = self.api()?;
            let dev = if path.is_empty() {
                api.open(vid, pid)
            } else {
                let path = std::ffi::CString::new(path).map_err(|e| e.to_string())?;
                api.open_path(&path)
            }
            .map_err(|e| e.to_string())?;
            let product = dev.get_product_string().ok().flatten().unwrap_or_default();
            Ok((Box::new(Device(dev)), product))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread::ThreadId;

    #[derive(Clone, Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<ThreadId>>>,
        names: Arc<Mutex<Vec<String>>>,
        written: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    struct Dev(Arc<Mutex<Vec<Vec<u8>>>>);

    impl Hid for Dev {
        fn write(&mut self, report: &[u8]) -> io::Result<()> {
            self.0.lock().unwrap().push(report.to_vec());
            Ok(())
        }
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            buf[0] = 7;
            Ok(1)
        }
    }

    impl Backend for Fake {
        fn enumerate(&mut self) -> Scan {
            self.calls.lock().unwrap().push(std::thread::current().id());
            Ok(self
                .names
                .lock()
                .unwrap()
                .iter()
                .map(|n| PortInfo::named(n))
                .collect())
        }
        fn open(&mut self, _path: &str) -> Result<(Box<dyn Hid>, String), String> {
            self.calls.lock().unwrap().push(std::thread::current().id());
            Ok((Box::new(Dev(self.written.clone())), "Ver.2.3".into()))
        }
    }

    #[test]
    fn devices_are_listed_and_driven_on_the_hid_thread() {
        let f = Fake::default();
        f.names.lock().unwrap().push("/dev/hidraw7".into());
        let mut hub = HidHub::spawn(f.clone());
        assert_eq!(listed(&hub.scan().unwrap()), ["/dev/hidraw7"]);
        let (mut hid, product) = hub.open("/dev/hidraw7").unwrap();
        assert_eq!(product, "Ver.2.3");
        hid.write(&[0, 1, 0xFE]).unwrap();
        let mut buf = [0; 8];
        assert_eq!(hid.read(&mut buf).unwrap(), 1);
        assert_eq!(*f.written.lock().unwrap(), [vec![0, 1, 0xFE]]);
        let me = std::thread::current().id();
        assert!(f.calls.lock().unwrap().iter().all(|t| *t != me));
        drop(hid);
    }
}
