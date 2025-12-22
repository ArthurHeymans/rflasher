//! SPI I/O modes

/// I/O mode for SPI transactions
///
/// Represents how data is transferred on the SPI bus, from single-wire
/// to quad-wire modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum IoMode {
    /// Standard SPI: 1-1-1 (cmd, addr, data all on single line)
    #[default]
    Single,
    /// Dual Output: 1-1-2 (data phase on 2 lines)
    DualOut,
    /// Dual I/O: 1-2-2 (addr and data on 2 lines)
    DualIo,
    /// Quad Output: 1-1-4 (data phase on 4 lines)
    QuadOut,
    /// Quad I/O: 1-4-4 (addr and data on 4 lines)
    QuadIo,
    /// QPI mode: 4-4-4 (everything on 4 lines)
    Qpi,
}

impl IoMode {
    /// Returns the number of data lines used for the command phase
    pub const fn cmd_lines(&self) -> u8 {
        match self {
            Self::Single | Self::DualOut | Self::DualIo | Self::QuadOut | Self::QuadIo => 1,
            Self::Qpi => 4,
        }
    }

    /// Returns the number of data lines used for the address phase
    pub const fn addr_lines(&self) -> u8 {
        match self {
            Self::Single | Self::DualOut | Self::QuadOut => 1,
            Self::DualIo => 2,
            Self::QuadIo | Self::Qpi => 4,
        }
    }

    /// Returns the number of data lines used for the data phase
    pub const fn data_lines(&self) -> u8 {
        match self {
            Self::Single => 1,
            Self::DualOut | Self::DualIo => 2,
            Self::QuadOut | Self::QuadIo | Self::Qpi => 4,
        }
    }

    /// Returns true if this mode uses multiple data lines
    pub const fn is_multi_io(&self) -> bool {
        !matches!(self, Self::Single)
    }
}
