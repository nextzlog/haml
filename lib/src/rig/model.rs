use crate::{AsciiCat, Dialect, Icom, Protocol, YaesuBinary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Icom { address: u8 },
    Ascii(Dialect),
    YaesuBinary,
}

impl Family {
    pub fn protocol(self) -> Box<dyn Protocol> {
        match self {
            Family::Icom { address } => Box::new(Icom::new(address)),
            Family::Ascii(d) => Box::new(AsciiCat::new(d)),
            Family::YaesuBinary => Box::new(YaesuBinary::new()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub name: &'static str,
    pub family: Family,
    pub baud: u32,
    pub stop_bits: u8,
    pub min_hz: u64,
    pub max_hz: u64,
}

impl Model {
    pub fn protocol(&self) -> Box<dyn Protocol> {
        self.family.protocol()
    }

    pub fn covers(&self, hz: u64) -> bool {
        (self.min_hz..=self.max_hz).contains(&hz)
    }

    pub fn find(name: &str) -> Option<&'static Model> {
        MODELS.iter().find(|m| m.name.eq_ignore_ascii_case(name))
    }
}

const HF: u64 = 54_000_000;
const UHF: u64 = 450_000_000;

const fn icom(name: &'static str, address: u8, max_hz: u64) -> Model {
    Model {
        name,
        family: Family::Icom { address },
        baud: 19200,
        stop_bits: 1,
        min_hz: 30_000,
        max_hz,
    }
}

const fn ascii(
    name: &'static str,
    dialect: Dialect,
    baud: u32,
    stop_bits: u8,
    max_hz: u64,
) -> Model {
    Model {
        name,
        family: Family::Ascii(dialect),
        baud,
        stop_bits,
        min_hz: 30_000,
        max_hz,
    }
}

const fn binary(name: &'static str, max_hz: u64) -> Model {
    Model {
        name,
        family: Family::YaesuBinary,
        baud: 9600,
        stop_bits: 2,
        min_hz: 100_000,
        max_hz,
    }
}

pub const MODELS: &[Model] = &[
    icom("IC-705", 0xA4, UHF),
    icom("IC-7300", 0x94, HF),
    icom("IC-7610", 0x98, HF),
    icom("IC-7851", 0x8E, HF),
    Model {
        min_hz: 144_000_000,
        ..icom("IC-9700", 0xA2, 1_300_000_000)
    },
    ascii("TS-590", Dialect::Kenwood, 9600, 1, HF),
    ascii("TS-890", Dialect::Kenwood, 9600, 1, HF),
    ascii("TS-990", Dialect::Kenwood, 9600, 1, HF),
    ascii("K3", Dialect::Elecraft, 38400, 1, HF),
    ascii("KX3", Dialect::Elecraft, 38400, 1, HF),
    ascii("FT-991A", Dialect::Yaesu, 38400, 2, UHF),
    ascii("FTDX10", Dialect::Yaesu, 38400, 2, HF),
    ascii("FT-710", Dialect::Yaesu, 38400, 1, HF),
    ascii("FTDX101", Dialect::Ftdx101, 38400, 2, HF),
    ascii("FTDX5000", Dialect::Ftdx5000, 38400, 2, HF),
    binary("FT-817", UHF),
    binary("FT-857", UHF),
    binary("FT-897", UHF),
];

pub fn poll_interval(baud: u32) -> std::time::Duration {
    std::time::Duration::from_millis(if baud <= 9600 { 150 } else { 75 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_found() {
        let names: std::collections::HashSet<_> = MODELS.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), MODELS.len());
        assert_eq!(
            Model::find("ic-7300").unwrap().family,
            Family::Icom { address: 0x94 }
        );
        assert!(Model::find("nope").is_none());
    }

    #[test]
    fn coverage() {
        let ic7300 = Model::find("IC-7300").unwrap();
        assert!(ic7300.covers(50_100_000) && !ic7300.covers(144_100_000));
        let ic9700 = Model::find("IC-9700").unwrap();
        assert!(!ic9700.covers(50_100_000) && ic9700.covers(1_294_000_000));
    }

    #[test]
    fn poll_interval_follows_the_speed() {
        assert_eq!(poll_interval(9600).as_millis(), 150);
        assert_eq!(poll_interval(19200).as_millis(), 75);
    }
}
