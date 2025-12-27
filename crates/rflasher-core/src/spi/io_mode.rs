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

    /// Returns true if this mode requires dual I/O capability
    pub const fn requires_dual(&self) -> bool {
        matches!(self, Self::DualOut | Self::DualIo)
    }

    /// Returns true if this mode requires quad I/O capability
    pub const fn requires_quad(&self) -> bool {
        matches!(self, Self::QuadOut | Self::QuadIo | Self::Qpi)
    }
}

use crate::error::{Error, Result};
use crate::programmer::SpiFeatures;

/// Check if a programmer supports the requested I/O mode
///
/// Returns `Ok(())` if the mode is supported, or `Err(IoModeNotSupported)` if not.
///
/// # Example
///
/// ```ignore
/// fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
///     check_io_mode_supported(cmd.io_mode, self.features())?;
///     // ... execute the command
/// }
/// ```
pub fn check_io_mode_supported(mode: IoMode, features: SpiFeatures) -> Result<()> {
    match mode {
        IoMode::Single => Ok(()),
        IoMode::DualOut => {
            if features.contains(SpiFeatures::DUAL_IN) {
                Ok(())
            } else {
                Err(Error::IoModeNotSupported)
            }
        }
        IoMode::DualIo => {
            if features.contains(SpiFeatures::DUAL_IO) {
                Ok(())
            } else {
                Err(Error::IoModeNotSupported)
            }
        }
        IoMode::QuadOut => {
            if features.contains(SpiFeatures::QUAD_IN) {
                Ok(())
            } else {
                Err(Error::IoModeNotSupported)
            }
        }
        IoMode::QuadIo => {
            if features.contains(SpiFeatures::QUAD_IO) {
                Ok(())
            } else {
                Err(Error::IoModeNotSupported)
            }
        }
        IoMode::Qpi => {
            if features.contains(SpiFeatures::QPI) {
                Ok(())
            } else {
                Err(Error::IoModeNotSupported)
            }
        }
    }
}
