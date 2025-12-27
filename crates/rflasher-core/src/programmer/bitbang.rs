//! Bitbang SPI master traits for multi-IO support
//!
//! This module provides traits for implementing **bitbang-style** SPI masters
//! with dual and quad I/O support.
//!
//! ## When to use these traits
//!
//! These traits are specifically for programmers that implement SPI via
//! software-controlled GPIO pins (bitbanging). Examples include:
//! - `linux_gpio` - Uses Linux GPIO character device
//! - `rpi_gpio` - Direct Raspberry Pi GPIO access
//! - Microcontroller GPIO implementations
//!
//! ## When NOT to use these traits
//!
//! Hardware-accelerated SPI programmers should **not** use these traits.
//! Instead, they should directly implement `SpiMaster::execute()` and handle
//! the `io_mode` field in `SpiCommand`. Examples include:
//! - `ft4222` - FTDI FT4222H with hardware multi-IO
//! - `ch347` - WCH CH347 with hardware SPI
//! - `linux_spi` - Linux spidev with hardware support
//!
//! ## Architecture
//!
//! The multi-IO support is designed as follows:
//!
//! 1. **SpiCommand** contains an `io_mode` field specifying the desired I/O mode
//! 2. **SpiMaster::execute()** is responsible for handling the command with the
//!    specified I/O mode
//! 3. **SpiFeatures** advertises what modes the programmer supports
//!
//! For bitbang programmers, this module provides:
//! - `BitbangSpiMaster` - Basic single-wire operations
//! - `BitbangDualIo` - Extended trait for dual I/O (2-bit parallel)
//! - `BitbangQuadIo` - Extended trait for quad I/O (4-bit parallel)
//! - Helper functions in `single`, `dual`, `quad` modules

use super::SpiFeatures;

/// Trait for low-level bitbang SPI operations
///
/// This trait provides the minimal set of operations needed for bitbanging SPI.
/// Implementations can optionally provide optimized multi-IO functions.
pub trait BitbangSpiMaster {
    /// Set chip select (CS is active low, so `active=true` means CS=0)
    fn set_cs(&mut self, active: bool);

    /// Set clock line value
    fn set_sck(&mut self, high: bool);

    /// Set MOSI/IO0 line value (for single-bit output)
    fn set_mosi(&mut self, high: bool);

    /// Get MISO/IO1 line value (for single-bit input)
    fn get_miso(&self) -> bool;

    /// Delay for half a clock period
    fn half_period_delay(&self);

    /// Optional: Set SCK and MOSI atomically (optimization)
    ///
    /// Default implementation calls `set_sck` then `set_mosi`.
    fn set_sck_set_mosi(&mut self, sck: bool, mosi: bool) {
        self.set_sck(sck);
        self.set_mosi(mosi);
    }

    /// Optional: Set SCK and get MISO atomically (optimization)
    ///
    /// Default implementation calls `set_sck` then `get_miso`.
    fn set_sck_get_miso(&mut self, sck: bool) -> bool {
        self.set_sck(sck);
        self.get_miso()
    }

    /// Optional: Request exclusive bus access
    fn request_bus(&mut self) {}

    /// Optional: Release bus access
    fn release_bus(&mut self) {}
}

/// Extended trait for dual I/O bitbanging operations
///
/// Implement this trait to enable dual I/O modes (1-1-2 and 1-2-2).
pub trait BitbangDualIo: BitbangSpiMaster {
    /// Set SCK and write 2 bits to IO0/IO1 atomically
    ///
    /// The `io` parameter contains 2 bits: bit 0 goes to IO0, bit 1 goes to IO1.
    fn set_sck_set_dual_io(&mut self, sck: bool, io: u8);

    /// Set SCK and read 2 bits from IO0/IO1 atomically
    ///
    /// Returns 2 bits: bit 0 from IO0, bit 1 from IO1.
    fn set_sck_get_dual_io(&mut self, sck: bool) -> u8;

    /// Set IO lines to idle/input state
    ///
    /// Called before switching from write to read in multi-IO modes.
    fn set_idle_io(&mut self);
}

/// Extended trait for quad I/O bitbanging operations
///
/// Implement this trait to enable quad I/O modes (1-1-4, 1-4-4, and 4-4-4).
/// Requires `BitbangDualIo` since quad mode uses all the same infrastructure.
pub trait BitbangQuadIo: BitbangDualIo {
    /// Set SCK and write 4 bits to IO0/IO1/IO2/IO3 atomically
    ///
    /// The `io` parameter contains 4 bits: bit 0 goes to IO0, bit 1 to IO1, etc.
    fn set_sck_set_quad_io(&mut self, sck: bool, io: u8);

    /// Set SCK and read 4 bits from IO0/IO1/IO2/IO3 atomically
    ///
    /// Returns 4 bits: bit 0 from IO0, bit 1 from IO1, etc.
    fn set_sck_get_quad_io(&mut self, sck: bool) -> u8;
}

/// Bitbang helper functions for single-wire I/O
///
/// These are standalone functions that can be used by any `BitbangSpiMaster` implementation.
pub mod single {
    use super::BitbangSpiMaster;

    /// Write a byte in single-wire mode (MSB first)
    pub fn write_byte<M: BitbangSpiMaster + ?Sized>(master: &mut M, byte: u8) {
        for i in (0..8).rev() {
            let bit = (byte >> i) & 1 != 0;
            master.set_sck_set_mosi(false, bit);
            master.half_period_delay();
            master.set_sck(true);
            master.half_period_delay();
        }
    }

    /// Read a byte in single-wire mode (MSB first)
    pub fn read_byte<M: BitbangSpiMaster + ?Sized>(master: &mut M) -> u8 {
        let mut byte = 0u8;
        for _ in 0..8 {
            master.set_sck(false);
            master.half_period_delay();
            byte <<= 1;
            if master.set_sck_get_miso(true) {
                byte |= 1;
            }
            master.half_period_delay();
        }
        byte
    }

    /// Run clock for a number of cycles (for dummy cycles)
    pub fn run_clock<M: BitbangSpiMaster + ?Sized>(master: &mut M, cycles: usize) {
        for _ in 0..cycles {
            master.set_sck(false);
            master.half_period_delay();
            master.set_sck(true);
            master.half_period_delay();
        }
    }

    /// Write multiple bytes in single-wire mode
    pub fn write_bytes<M: BitbangSpiMaster + ?Sized>(master: &mut M, bytes: &[u8]) {
        for &byte in bytes {
            write_byte(master, byte);
        }
    }

    /// Read multiple bytes in single-wire mode
    pub fn read_bytes<M: BitbangSpiMaster + ?Sized>(master: &mut M, buf: &mut [u8]) {
        for byte in buf.iter_mut() {
            *byte = read_byte(master);
        }
    }
}

/// Bitbang helper functions for dual I/O
pub mod dual {
    use super::BitbangDualIo;

    /// Write a byte in dual mode (4 clocks, 2 bits per clock, MSB first)
    pub fn write_byte<M: BitbangDualIo + ?Sized>(master: &mut M, byte: u8) {
        // MSB first: bits 7-6, then 5-4, then 3-2, then 1-0
        for shift in (0..8).rev().step_by(2) {
            let bits = (byte >> shift) & 0x3;
            master.set_sck_set_dual_io(false, bits);
            master.half_period_delay();
            master.set_sck(true);
            master.half_period_delay();
        }
    }

    /// Read a byte in dual mode (4 clocks, 2 bits per clock, MSB first)
    pub fn read_byte<M: BitbangDualIo + ?Sized>(master: &mut M) -> u8 {
        let mut byte = 0u8;
        for _ in 0..4 {
            master.set_sck(false);
            master.half_period_delay();
            byte <<= 2;
            byte |= master.set_sck_get_dual_io(true) & 0x3;
            master.half_period_delay();
        }
        byte
    }

    /// Write multiple bytes in dual mode
    pub fn write_bytes<M: BitbangDualIo + ?Sized>(master: &mut M, bytes: &[u8]) {
        for &byte in bytes {
            write_byte(master, byte);
        }
    }

    /// Read multiple bytes in dual mode
    pub fn read_bytes<M: BitbangDualIo + ?Sized>(master: &mut M, buf: &mut [u8]) {
        for byte in buf.iter_mut() {
            *byte = read_byte(master);
        }
    }
}

/// Bitbang helper functions for quad I/O
pub mod quad {
    use super::BitbangQuadIo;

    /// Write a byte in quad mode (2 clocks, 4 bits per clock, MSB first)
    pub fn write_byte<M: BitbangQuadIo + ?Sized>(master: &mut M, byte: u8) {
        // MSB first: high nibble then low nibble
        let high = (byte >> 4) & 0xF;
        let low = byte & 0xF;

        master.set_sck_set_quad_io(false, high);
        master.half_period_delay();
        master.set_sck(true);
        master.half_period_delay();

        master.set_sck_set_quad_io(false, low);
        master.half_period_delay();
        master.set_sck(true);
        master.half_period_delay();
    }

    /// Read a byte in quad mode (2 clocks, 4 bits per clock, MSB first)
    pub fn read_byte<M: BitbangQuadIo + ?Sized>(master: &mut M) -> u8 {
        let mut byte = 0u8;
        for _ in 0..2 {
            master.set_sck(false);
            master.half_period_delay();
            byte <<= 4;
            byte |= master.set_sck_get_quad_io(true) & 0xF;
            master.half_period_delay();
        }
        byte
    }

    /// Write multiple bytes in quad mode
    pub fn write_bytes<M: BitbangQuadIo + ?Sized>(master: &mut M, bytes: &[u8]) {
        for &byte in bytes {
            write_byte(master, byte);
        }
    }

    /// Read multiple bytes in quad mode
    pub fn read_bytes<M: BitbangQuadIo + ?Sized>(master: &mut M, buf: &mut [u8]) {
        for byte in buf.iter_mut() {
            *byte = read_byte(master);
        }
    }
}

/// Get the SPI features based on which traits are implemented
///
/// Call this with the most specific trait bound available.
pub fn features_for_single() -> SpiFeatures {
    SpiFeatures::FOUR_BYTE_ADDR
}

/// Get features for dual I/O capable master
pub fn features_for_dual() -> SpiFeatures {
    SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::DUAL
}

/// Get features for quad I/O capable master
pub fn features_for_quad() -> SpiFeatures {
    SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::DUAL | SpiFeatures::QUAD | SpiFeatures::QPI
}
