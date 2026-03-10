use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Attr: u8 {
        const BOLD       = 0b0000_0001;
        const DIM        = 0b0000_0010;
        const ITALIC     = 0b0000_0100;
        const UNDERLINE  = 0b0000_1000;
        const BLINK      = 0b0001_0000;
        const REVERSE    = 0b0010_0000;
        const HIDDEN     = 0b0100_0000;
        const STRIKE     = 0b1000_0000;
    }
}

impl Serialize for Attr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.bits().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Attr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bits = u8::deserialize(deserializer)?;
        Ok(Attr::from_bits_truncate(bits))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Default for Color {
    fn default() -> Self {
        Color::Default
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub attr: Attr,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            c: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attr: Attr::empty(),
        }
    }
}
