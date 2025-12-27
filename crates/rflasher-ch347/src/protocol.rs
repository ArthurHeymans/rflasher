//! CH347 protocol constants and helpers
//!
//! This module contains the USB protocol constants and configuration
//! structures needed to communicate with the CH347.

// Allow unused constants - these are register/command definitions for documentation
// and potential future use
#![allow(dead_code)]

// USB device identifiers
/// CH347T USB VID
pub const CH347_USB_VENDOR: u16 = 0x1A86;
/// CH347T USB PID (USB to UART+SPI+I2C)
pub const CH347T_USB_PRODUCT: u16 = 0x55DB;
/// CH347F USB PID (USB to UART+SPI+I2C+JTAG)
pub const CH347F_USB_PRODUCT: u16 = 0x55DE;

// USB endpoints (different from CH341A)
/// Bulk OUT endpoint for sending commands/data
pub const WRITE_EP: u8 = 0x06;
/// Bulk IN endpoint for receiving responses/data
pub const READ_EP: u8 = 0x86;

// USB timeout in milliseconds
pub const USB_TIMEOUT_MS: u64 = 1000;

// Packet sizes
/// Maximum USB packet size (USB descriptor says 512, but vendor driver uses 510)
pub const CH347_PACKET_SIZE: usize = 510;
/// Maximum data length per packet (packet size - 3 bytes for command header)
pub const CH347_MAX_DATA_LEN: usize = CH347_PACKET_SIZE - 3;

// Command codes
/// Set SPI configuration
pub const CH347_CMD_SPI_SET_CFG: u8 = 0xC0;
/// Control chip select lines
pub const CH347_CMD_SPI_CS_CTRL: u8 = 0xC1;
/// SPI bidirectional transfer (write then read)
pub const CH347_CMD_SPI_OUT_IN: u8 = 0xC2;
/// SPI read-only transfer
pub const CH347_CMD_SPI_IN: u8 = 0xC3;
/// SPI write-only transfer
pub const CH347_CMD_SPI_OUT: u8 = 0xC4;
/// Get current SPI configuration
pub const CH347_CMD_SPI_GET_CFG: u8 = 0xCA;

// Chip select control flags
/// Assert (activate) chip select
pub const CH347_CS_ASSERT: u8 = 0x00;
/// Deassert (deactivate) chip select
pub const CH347_CS_DEASSERT: u8 = 0x40;
/// Actually change the CS state (required flag)
pub const CH347_CS_CHANGE: u8 = 0x80;
/// Ignore this CS line (for multi-chip configurations)
pub const CH347_CS_IGNORE: u8 = 0x00;

/// SPI clock divisor settings
///
/// The CH347 has a 120MHz base clock, divided by powers of 2.
/// Divisor = 2^(value + 1), so:
/// - 0 -> 60 MHz
/// - 1 -> 30 MHz
/// - 2 -> 15 MHz
/// - 3 -> 7.5 MHz
/// - 4 -> 3.75 MHz
/// - 5 -> 1.875 MHz
/// - 6 -> 937.5 kHz
/// - 7 -> 468.75 kHz
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpiSpeed {
    /// 60 MHz (divisor 0)
    Speed60M = 0,
    /// 30 MHz (divisor 1)
    Speed30M = 1,
    /// 15 MHz (divisor 2)
    Speed15M = 2,
    /// 7.5 MHz (divisor 3, default - safe for most chips)
    #[default]
    Speed7_5M = 3,
    /// 3.75 MHz (divisor 4)
    Speed3_75M = 4,
    /// 1.875 MHz (divisor 5)
    Speed1_875M = 5,
    /// 937.5 kHz (divisor 6)
    Speed937_5K = 6,
    /// 468.75 kHz (divisor 7)
    Speed468_75K = 7,
}

impl SpiSpeed {
    /// Convert a frequency in kHz to the closest divisor
    pub fn from_khz(khz: u32) -> Self {
        // Base clock is 120 MHz, divisor = 2^(n+1)
        // Find the smallest divisor that doesn't exceed the requested speed
        const BASE_KHZ: u32 = 120_000;
        for div in 0..=7 {
            let speed = BASE_KHZ / (1 << (div + 1));
            if speed <= khz {
                return match div {
                    0 => SpiSpeed::Speed60M,
                    1 => SpiSpeed::Speed30M,
                    2 => SpiSpeed::Speed15M,
                    3 => SpiSpeed::Speed7_5M,
                    4 => SpiSpeed::Speed3_75M,
                    5 => SpiSpeed::Speed1_875M,
                    6 => SpiSpeed::Speed937_5K,
                    _ => SpiSpeed::Speed468_75K,
                };
            }
        }
        SpiSpeed::Speed468_75K
    }

    /// Get the actual speed in kHz for this divisor
    pub fn to_khz(self) -> u32 {
        const BASE_KHZ: u32 = 120_000;
        BASE_KHZ / (1 << (self as u32 + 1))
    }

    /// Get the divisor value for the configuration register
    pub fn divisor(self) -> u8 {
        self as u8
    }
}

/// SPI mode (clock polarity and phase)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpiMode {
    /// Mode 0: CPOL=0, CPHA=0 (clock idle low, sample on rising edge)
    #[default]
    Mode0 = 0,
    /// Mode 1: CPOL=0, CPHA=1 (clock idle low, sample on falling edge)
    Mode1 = 1,
    /// Mode 2: CPOL=1, CPHA=0 (clock idle high, sample on falling edge)
    Mode2 = 2,
    /// Mode 3: CPOL=1, CPHA=1 (clock idle high, sample on rising edge)
    Mode3 = 3,
}

impl SpiMode {
    /// Get clock polarity (CPOL)
    pub fn cpol(self) -> u8 {
        (self as u8 >> 1) & 1
    }

    /// Get clock phase (CPHA)
    pub fn cpha(self) -> u8 {
        self as u8 & 1
    }
}

/// Which chip select line to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChipSelect {
    /// Use CS0 (default)
    #[default]
    CS0 = 0,
    /// Use CS1
    CS1 = 1,
}

/// SPI configuration for CH347
#[derive(Debug, Clone, Default)]
pub struct SpiConfig {
    /// SPI clock speed
    pub speed: SpiSpeed,
    /// SPI mode (clock polarity and phase)
    pub mode: SpiMode,
    /// Which chip select to use
    pub cs: ChipSelect,
    /// Bit order: false = MSB first (standard), true = LSB first
    pub lsb_first: bool,
}

impl SpiConfig {
    /// Create a new SPI configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the SPI clock speed
    pub fn with_speed(mut self, speed: SpiSpeed) -> Self {
        self.speed = speed;
        self
    }

    /// Set the SPI clock speed from a frequency in kHz
    pub fn with_speed_khz(mut self, khz: u32) -> Self {
        self.speed = SpiSpeed::from_khz(khz);
        self
    }

    /// Set the SPI mode
    pub fn with_mode(mut self, mode: SpiMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set which chip select to use
    pub fn with_cs(mut self, cs: ChipSelect) -> Self {
        self.cs = cs;
        self
    }

    /// Build the 29-byte configuration buffer for CH347_CMD_SPI_SET_CFG
    pub fn build_config_buffer(&self) -> [u8; 29] {
        let mut buf = [0u8; 29];

        buf[0] = CH347_CMD_SPI_SET_CFG;
        // Payload length (26 bytes = 29 - 3 header bytes)
        buf[1] = 26;
        buf[2] = 0;

        // Mystery bytes - vendor driver sets these unconditionally
        buf[5] = 4;
        buf[6] = 1;

        // Clock polarity: bit 1 at offset 9
        buf[9] = self.mode.cpol() << 1;

        // Clock phase: bit 0 at offset 11
        buf[11] = self.mode.cpha();

        // Another mystery byte
        buf[14] = 2;

        // Clock divisor: bits 5:3 at offset 15
        buf[15] = (self.speed.divisor() & 0x7) << 3;

        // Bit order: bit 7 at offset 17, 0 = MSB first
        buf[17] = if self.lsb_first { 0x80 } else { 0x00 };

        // Yet another mystery byte
        buf[19] = 7;

        // CS polarity: bit 7 = CS2, bit 6 = CS1. 0 = active low
        buf[24] = 0;

        buf
    }
}

/// CH347 variant information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ch347Variant {
    /// CH347T: USB to UART+SPI+I2C
    Ch347T,
    /// CH347F: USB to UART+SPI+I2C+JTAG
    Ch347F,
}

impl Ch347Variant {
    /// Get the USB product ID for this variant
    pub fn product_id(self) -> u16 {
        match self {
            Ch347Variant::Ch347T => CH347T_USB_PRODUCT,
            Ch347Variant::Ch347F => CH347F_USB_PRODUCT,
        }
    }

    /// Detect variant from USB product ID
    pub fn from_product_id(pid: u16) -> Option<Self> {
        match pid {
            CH347T_USB_PRODUCT => Some(Ch347Variant::Ch347T),
            CH347F_USB_PRODUCT => Some(Ch347Variant::Ch347F),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spi_speed_from_khz() {
        // Exact matches
        assert_eq!(SpiSpeed::from_khz(60_000), SpiSpeed::Speed60M);
        assert_eq!(SpiSpeed::from_khz(30_000), SpiSpeed::Speed30M);
        assert_eq!(SpiSpeed::from_khz(15_000), SpiSpeed::Speed15M);

        // Higher than max should give max
        assert_eq!(SpiSpeed::from_khz(100_000), SpiSpeed::Speed60M);

        // Between values should round down
        assert_eq!(SpiSpeed::from_khz(20_000), SpiSpeed::Speed15M);
        assert_eq!(SpiSpeed::from_khz(10_000), SpiSpeed::Speed7_5M);

        // Very low should give minimum
        assert_eq!(SpiSpeed::from_khz(100), SpiSpeed::Speed468_75K);
    }

    #[test]
    fn test_spi_speed_to_khz() {
        assert_eq!(SpiSpeed::Speed60M.to_khz(), 60_000);
        assert_eq!(SpiSpeed::Speed30M.to_khz(), 30_000);
        assert_eq!(SpiSpeed::Speed15M.to_khz(), 15_000);
        assert_eq!(SpiSpeed::Speed7_5M.to_khz(), 7_500);
    }

    #[test]
    fn test_spi_mode() {
        assert_eq!(SpiMode::Mode0.cpol(), 0);
        assert_eq!(SpiMode::Mode0.cpha(), 0);
        assert_eq!(SpiMode::Mode1.cpol(), 0);
        assert_eq!(SpiMode::Mode1.cpha(), 1);
        assert_eq!(SpiMode::Mode2.cpol(), 1);
        assert_eq!(SpiMode::Mode2.cpha(), 0);
        assert_eq!(SpiMode::Mode3.cpol(), 1);
        assert_eq!(SpiMode::Mode3.cpha(), 1);
    }

    #[test]
    fn test_config_buffer() {
        let config = SpiConfig::new();
        let buf = config.build_config_buffer();

        assert_eq!(buf[0], CH347_CMD_SPI_SET_CFG);
        assert_eq!(buf[1], 26); // payload length
        assert_eq!(buf[5], 4); // mystery byte
        assert_eq!(buf[6], 1); // mystery byte
    }
}
