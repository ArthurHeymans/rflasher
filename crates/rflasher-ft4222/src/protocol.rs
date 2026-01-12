//! FT4222H protocol constants and helpers
//!
//! This module contains the USB protocol constants and configuration
//! structures needed to communicate with the FT4222H.
//!
//! The FT4222H uses a vendor-specific USB protocol (no libftdi/MPSSE).
//! All communication is done via control and bulk transfers.

// Allow unused constants - these are register/command definitions for documentation
// and potential future use
#![allow(dead_code)]

// ============================================================================
// USB device identifiers
// ============================================================================

/// FTDI vendor ID
pub const FTDI_VID: u16 = 0x0403;
/// FT4222H product ID
pub const FT4222H_PID: u16 = 0x601C;

// ============================================================================
// USB endpoints (determined from USB descriptor)
// ============================================================================

/// USB control request types
pub const USB_REQ_TYPE_VENDOR_OUT: u8 = 0x40;
pub const USB_REQ_TYPE_VENDOR_IN: u8 = 0xC0;

/// USB request codes for FT4222H
pub const FT4222_RESET_REQUEST: u8 = 0x00;
pub const FT4222_INFO_REQUEST: u8 = 0x20;
pub const FT4222_CONFIG_REQUEST: u8 = 0x21;

/// Reset command values (wValue for RESET_REQUEST)
pub const FT4222_RESET_SIO: u16 = 0x0000;
pub const FT4222_OUTPUT_FLUSH: u16 = 0x0001;
pub const FT4222_INPUT_FLUSH: u16 = 0x0002;

/// Info command values (wValue for INFO_REQUEST)
pub const FT4222_GET_VERSION: u16 = 0x0000;
pub const FT4222_GET_CONFIG: u16 = 0x0001;

/// Config command codes (lower byte of wValue for CONFIG_REQUEST)
/// The data byte goes in the upper byte: wValue = (data << 8) | cmd
pub const FT4222_SET_CLOCK: u8 = 0x04;
pub const FT4222_SET_MODE: u8 = 0x05;
pub const FT4222_SPI_SET_IO_LINES: u8 = 0x42;
pub const FT4222_SPI_SET_CS_ACTIVE: u8 = 0x43;
pub const FT4222_SPI_SET_CLK_DIV: u8 = 0x44;
pub const FT4222_SPI_SET_CLK_IDLE: u8 = 0x45;
pub const FT4222_SPI_SET_CAPTURE: u8 = 0x46;
pub const FT4222_SPI_SET_CS_MASK: u8 = 0x48;
pub const FT4222_SPI_RESET_TRANSACTION: u8 = 0x49;
pub const FT4222_SPI_RESET: u8 = 0x4A;

/// SPI reset types (data byte for FT4222_SPI_RESET)
pub const FT4222_SPI_RESET_FULL: u8 = 0;
pub const FT4222_SPI_RESET_LINE_NUM: u8 = 1;

/// Mode values (data byte for SET_MODE)
pub const FT4222_MODE_SPI_MASTER: u8 = 3;

/// Clock polarity and phase (data bytes)
pub const FT4222_CLK_IDLE_LOW: u8 = 0;
pub const FT4222_CLK_IDLE_HIGH: u8 = 1;
pub const FT4222_CLK_CAPTURE_LEADING: u8 = 0;
pub const FT4222_CLK_CAPTURE_TRAILING: u8 = 1;

/// CS polarity (data byte for SPI_SET_CS_ACTIVE)
pub const FT4222_CS_ACTIVE_LOW: u8 = 0;
pub const FT4222_CS_ACTIVE_HIGH: u8 = 1;

// ============================================================================
// Buffer and transfer sizes
// ============================================================================

/// Maximum USB packet size (high-speed bulk: 512 bytes)
/// Note: FT4222 uses 512-byte packets with 2-byte modem status header
pub const USB_PACKET_SIZE: usize = 512;

/// Modem status bytes at the start of each IN packet
pub const MODEM_STATUS_SIZE: usize = 2;

/// Maximum payload per USB packet (excluding modem status)
pub const USB_PAYLOAD_SIZE: usize = USB_PACKET_SIZE - MODEM_STATUS_SIZE;

/// Read buffer size for async transfers
pub const READ_BUFFER_SIZE: usize = 2048;

/// Maximum concurrent read transfers
pub const READ_MAX_XFERS: usize = 4;

/// Default SPI clock speed in kHz (10 MHz)
pub const DEFAULT_SPI_SPEED_KHZ: u32 = 10_000;

// ============================================================================
// Multi-I/O header format
// ============================================================================

/// Multi-I/O header size (5 bytes)
/// Format: | 4-bit 0x8 | 4-bit single_len | 2B multi_write_len | 2B multi_read_len |
pub const MULTI_IO_HEADER_SIZE: usize = 5;

/// Multi-I/O header magic nibble
pub const MULTI_IO_MAGIC: u8 = 0x80;

/// Maximum single-I/O bytes in multi-I/O command (4 bits = 0-15)
pub const MULTI_IO_MAX_SINGLE: usize = 15;

/// Maximum multi-I/O bytes in each direction (16 bits = 0-65535)
pub const MULTI_IO_MAX_DATA: usize = 65535;

// ============================================================================
// Clock configuration
// ============================================================================

/// System clock options (base frequencies)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemClock {
    /// 60 MHz system clock
    Clock60MHz = 0,
    /// 24 MHz system clock
    Clock24MHz = 1,
    /// 48 MHz system clock
    Clock48MHz = 2,
    /// 80 MHz system clock
    Clock80MHz = 3,
}

impl SystemClock {
    /// Get the frequency in kHz
    pub fn to_khz(self) -> u32 {
        match self {
            SystemClock::Clock60MHz => 60_000,
            SystemClock::Clock24MHz => 24_000,
            SystemClock::Clock48MHz => 48_000,
            SystemClock::Clock80MHz => 80_000,
        }
    }

    /// Get the register index value
    pub fn index(self) -> u16 {
        self as u16
    }
}

/// Clock divisor (power of 2)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockDivisor {
    /// Divide by 2
    Div2 = 1,
    /// Divide by 4
    Div4 = 2,
    /// Divide by 8
    Div8 = 3,
    /// Divide by 16
    Div16 = 4,
    /// Divide by 32
    Div32 = 5,
    /// Divide by 64
    Div64 = 6,
    /// Divide by 128
    Div128 = 7,
    /// Divide by 256
    Div256 = 8,
    /// Divide by 512
    Div512 = 9,
}

impl ClockDivisor {
    /// Get the actual divisor value
    pub fn divisor(self) -> u32 {
        1 << (self as u32)
    }

    /// Get the register value
    pub fn value(self) -> u16 {
        self as u16
    }
}

/// Complete clock configuration
#[derive(Debug, Clone, Copy)]
pub struct ClockConfig {
    /// System clock selection
    pub sys_clock: SystemClock,
    /// Clock divisor
    pub divisor: ClockDivisor,
}

impl ClockConfig {
    /// Calculate the resulting SPI clock frequency in kHz
    pub fn spi_clock_khz(&self) -> u32 {
        self.sys_clock.to_khz() / self.divisor.divisor()
    }
}

/// Find the best clock configuration for a target speed
///
/// Returns the configuration that gives the highest speed not exceeding target_khz.
/// Based on flashprog's `ft4222_find_spi_clock` algorithm.
pub fn find_clock_config(target_khz: u32) -> ClockConfig {
    // Available system clocks in order of preference
    // We prefer 60MHz because it gives the most flexibility
    const SYS_CLOCKS: [SystemClock; 4] = [
        SystemClock::Clock60MHz,
        SystemClock::Clock80MHz,
        SystemClock::Clock48MHz,
        SystemClock::Clock24MHz,
    ];

    const DIVISORS: [ClockDivisor; 9] = [
        ClockDivisor::Div2,
        ClockDivisor::Div4,
        ClockDivisor::Div8,
        ClockDivisor::Div16,
        ClockDivisor::Div32,
        ClockDivisor::Div64,
        ClockDivisor::Div128,
        ClockDivisor::Div256,
        ClockDivisor::Div512,
    ];

    let mut best: Option<ClockConfig> = None;
    let mut best_khz: u32 = 0;

    for &sys_clock in &SYS_CLOCKS {
        for &divisor in &DIVISORS {
            let speed = sys_clock.to_khz() / divisor.divisor();
            if speed <= target_khz && speed > best_khz {
                best = Some(ClockConfig { sys_clock, divisor });
                best_khz = speed;
            }
        }
    }

    // If no suitable config found, use the slowest possible
    best.unwrap_or(ClockConfig {
        sys_clock: SystemClock::Clock24MHz,
        divisor: ClockDivisor::Div512,
    })
}

// ============================================================================
// I/O mode configuration
// ============================================================================

/// I/O mode for SPI transfers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IoMode {
    /// Single I/O (standard SPI: 1-1-1)
    #[default]
    Single = 1,
    /// Dual I/O (1-1-2 or 1-2-2)
    Dual = 2,
    /// Quad I/O (1-1-4 or 1-4-4 or 4-4-4)
    Quad = 4,
}

impl IoMode {
    /// Get the number of I/O lines
    pub fn lines(self) -> u8 {
        self as u8
    }

    /// Parse from string
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "single" | "1" => Some(IoMode::Single),
            "dual" | "2" => Some(IoMode::Dual),
            "quad" | "4" => Some(IoMode::Quad),
            _ => None,
        }
    }
}

// ============================================================================
// SPI configuration
// ============================================================================

/// Complete SPI configuration for FT4222H
#[derive(Debug, Clone)]
pub struct SpiConfig {
    /// Chip select number (0-3)
    pub cs: u8,
    /// Target SPI speed in kHz
    pub speed_khz: u32,
    /// I/O mode (single/dual/quad)
    pub io_mode: IoMode,
}

impl Default for SpiConfig {
    fn default() -> Self {
        Self {
            cs: 0,
            speed_khz: DEFAULT_SPI_SPEED_KHZ,
            io_mode: IoMode::Single,
        }
    }
}

impl SpiConfig {
    /// Create a new configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the chip select number
    pub fn with_cs(mut self, cs: u8) -> Self {
        self.cs = cs;
        self
    }

    /// Set the SPI speed in kHz
    pub fn with_speed_khz(mut self, speed: u32) -> Self {
        self.speed_khz = speed;
        self
    }

    /// Set the I/O mode
    pub fn with_io_mode(mut self, mode: IoMode) -> Self {
        self.io_mode = mode;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_clock_config() {
        // 10 MHz target
        let config = find_clock_config(10_000);
        let actual = config.spi_clock_khz();
        assert!(actual <= 10_000);
        assert!(actual >= 7_500); // Should be reasonably close

        // 30 MHz target (max reasonable for FT4222H)
        let config = find_clock_config(30_000);
        let actual = config.spi_clock_khz();
        assert!(actual <= 30_000);
        assert!(actual >= 20_000);

        // Very low speed
        let config = find_clock_config(100);
        let actual = config.spi_clock_khz();
        assert!(actual <= 100 || actual == 24_000 / 512); // Minimum possible
    }

    #[test]
    fn test_system_clock_values() {
        assert_eq!(SystemClock::Clock60MHz.to_khz(), 60_000);
        assert_eq!(SystemClock::Clock24MHz.to_khz(), 24_000);
        assert_eq!(SystemClock::Clock48MHz.to_khz(), 48_000);
        assert_eq!(SystemClock::Clock80MHz.to_khz(), 80_000);
    }

    #[test]
    fn test_divisor_values() {
        assert_eq!(ClockDivisor::Div2.divisor(), 2);
        assert_eq!(ClockDivisor::Div4.divisor(), 4);
        assert_eq!(ClockDivisor::Div512.divisor(), 512);
    }

    #[test]
    fn test_io_mode_parse() {
        assert_eq!(IoMode::parse("single"), Some(IoMode::Single));
        assert_eq!(IoMode::parse("dual"), Some(IoMode::Dual));
        assert_eq!(IoMode::parse("quad"), Some(IoMode::Quad));
        assert_eq!(IoMode::parse("1"), Some(IoMode::Single));
        assert_eq!(IoMode::parse("2"), Some(IoMode::Dual));
        assert_eq!(IoMode::parse("4"), Some(IoMode::Quad));
        assert_eq!(IoMode::parse("invalid"), None);
    }
}
