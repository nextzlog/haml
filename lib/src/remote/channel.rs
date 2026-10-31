use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use super::{fan, listen, Client, Fanout};
use crate::{Error, Result};

pub const KEPT: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Open(String),
    Line(String),
    Closed,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct Channel {
    name: String,
    inbox: Arc<Mutex<VecDeque<Inbound>>>,
    outbox: Fanout<String>,
    wake: Option<Wake>,
}

impl Channel {
    pub fn new(name: &str) -> Self {
        Channel {
            name: super::wire::clean(name),
            inbox: Arc::default(),
            outbox: Fanout::default(),
            wake: None,
        }
    }

    pub fn waking(mut self, wake: impl Fn() + Send + Sync + 'static) -> Self {
        self.wake = Some(Arc::new(wake));
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn take(&self) -> Vec<Inbound> {
        self.inbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    pub fn say(&self, line: &str) {
        fan(&self.outbox, super::wire::clean(line));
    }

    pub(super) fn push(&self, i: Inbound) {
        let mut q = self.inbox.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= KEPT {
            q.pop_front();
        }
        q.push_back(i);
        drop(q);
        if let Some(w) = &self.wake {
            w();
        }
    }

    pub(super) fn listen(&self) -> Receiver<String> {
        listen(&self.outbox)
    }
}

pub struct RemoteChannel {
    pub(super) client: Client,
    pub(super) index: usize,
    pub(super) events: Receiver<String>,
}

impl RemoteChannel {
    pub fn send(&self, line: &str) -> Result<()> {
        let line = format!("APP {} {}", self.index, super::wire::clean(line));
        self.client.line(&line).map_err(|_| Error::Disconnected)
    }

    pub fn recv(&self) -> Vec<String> {
        self.events.try_iter().collect()
    }

    pub fn connected(&self) -> bool {
        self.client.connected()
    }
}
