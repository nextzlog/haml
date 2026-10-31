use std::io;

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Dtr,
    Rts,
}

impl Line {
    pub fn other(self) -> Line {
        match self {
            Line::Dtr => Line::Rts,
            Line::Rts => Line::Dtr,
        }
    }
}

pub trait Lines: Send + 'static {
    fn set(&mut self, line: Line, on: bool) -> io::Result<()>;
}

impl<L: Lines + ?Sized> Lines for Box<L> {
    fn set(&mut self, line: Line, on: bool) -> io::Result<()> {
        (**self).set(line, on)
    }
}

pub trait Ptt: Send {
    fn set_ptt(&mut self, on: bool) -> Result<()>;

    fn ptt(&self) -> bool;
}

impl<P: Ptt + ?Sized> Ptt for Box<P> {
    fn set_ptt(&mut self, on: bool) -> Result<()> {
        (**self).set_ptt(on)
    }

    fn ptt(&self) -> bool {
        (**self).ptt()
    }
}

pub struct LinePtt<L: Lines> {
    lines: L,
    line: Line,
    on: bool,
}

impl<L: Lines> LinePtt<L> {
    pub fn new(lines: L, line: Line) -> Self {
        LinePtt {
            lines,
            line,
            on: false,
        }
    }
}

impl<L: Lines> Ptt for LinePtt<L> {
    fn set_ptt(&mut self, on: bool) -> Result<()> {
        self.lines.set(self.line, on)?;
        self.on = on;
        Ok(())
    }

    fn ptt(&self) -> bool {
        self.on
    }
}

impl<L: Lines> Drop for LinePtt<L> {
    fn drop(&mut self) {
        if self.on {
            let _ = self.lines.set(self.line, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Vec<(Line, bool)>>>);

    impl Lines for Fake {
        fn set(&mut self, line: Line, on: bool) -> io::Result<()> {
            self.0.lock().unwrap().push((line, on));
            Ok(())
        }
    }

    #[test]
    fn a_line_keys_ptt_and_is_released_on_drop() {
        let f = Fake::default();
        let mut p = LinePtt::new(f.clone(), Line::Rts);
        p.set_ptt(true).unwrap();
        assert!(p.ptt());
        drop(p);
        assert_eq!(
            *f.0.lock().unwrap(),
            [(Line::Rts, true), (Line::Rts, false)]
        );
        assert_eq!(Line::Dtr.other(), Line::Rts);
    }
}
