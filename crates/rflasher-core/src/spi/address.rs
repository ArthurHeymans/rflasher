//! Address width types

/// Address width for SPI commands
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AddressWidth {
    /// No address phase
    #[default]
    None,
    /// 3-byte (24-bit) address - supports up to 16 MiB
    ThreeByte,
    /// 4-byte (32-bit) address - supports up to 4 GiB
    FourByte,
}

impl AddressWidth {
    /// Returns the number of address bytes
    pub const fn bytes(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::ThreeByte => 3,
            Self::FourByte => 4,
        }
    }

    /// Returns the maximum addressable size in bytes
    pub const fn max_size(&self) -> u32 {
        match self {
            Self::None => 0,
            Self::ThreeByte => 16 * 1024 * 1024, // 16 MiB
            Self::FourByte => u32::MAX,          // ~4 GiB
        }
    }

    /// Encode an address into bytes
    pub fn encode(&self, address: u32, buf: &mut [u8]) {
        match self {
            Self::None => {}
            Self::ThreeByte => {
                buf[0] = (address >> 16) as u8;
                buf[1] = (address >> 8) as u8;
                buf[2] = address as u8;
            }
            Self::FourByte => {
                buf[0] = (address >> 24) as u8;
                buf[1] = (address >> 16) as u8;
                buf[2] = (address >> 8) as u8;
                buf[3] = address as u8;
            }
        }
    }
}
