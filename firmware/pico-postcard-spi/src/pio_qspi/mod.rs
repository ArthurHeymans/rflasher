//! PIO-based multi-I/O SPI driver for RP2040
//!
//! This module implements a flexible SPI driver using the RP2040's PIO
//! peripheral, supporting standard SPI (1-1-1) as well as multi-I/O modes:
//!
//! - 1-1-2 (Dual Output): Data read on 2 lines
//! - 1-2-2 (Dual I/O): Address and data on 2 lines
//! - 1-1-4 (Quad Output): Data read on 4 lines
//! - 1-4-4 (Quad I/O): Address and data on 4 lines
//! - 4-4-4 (QPI): Everything on 4 lines
//!
//! ## Pin Assignments
//!
//! | Pin | Function |
//! |-----|----------|
//! | SCK | SPI clock (directly controlled) |
//! | IO0 | MOSI / D0 |
//! | IO1 | MISO / D1 |
//! | IO2 | D2 (WP# in single mode) |
//! | IO3 | D3 (HOLD# in single mode) |
//!
//! ## Implementation Strategy
//!
//! For simplicity and flexibility, we implement a bit-banged approach using
//! PIO that can switch between modes dynamically. While this may be slower
//! than dedicated PIO programs for each mode, it's more flexible and easier
//! to debug.
//!
//! For higher performance, dedicated PIO programs could be loaded for each
//! mode, but this initial implementation prioritizes correctness and
//! flexibility.

use embassy_rp::gpio::{Flex, Pin, Pull};
use embassy_rp::Peri;
use postcard_spi_icd::IoMode;

/// PIO-based QSPI driver
pub struct PioQspi {
    /// Clock pin (directly controlled GPIO for now)
    sck: Flex<'static>,
    /// IO0 (MOSI in single mode)
    io0: Flex<'static>,
    /// IO1 (MISO in single mode)
    io1: Flex<'static>,
    /// IO2
    io2: Flex<'static>,
    /// IO3
    io3: Flex<'static>,
    /// Clock divider (half-periods per bit)
    clock_div: u16,
}

impl PioQspi {
    /// Create a new PIO QSPI driver
    ///
    /// Note: This initial implementation uses bit-banging via GPIO for
    /// simplicity. A future optimization would use PIO state machines
    /// for higher throughput.
    pub fn new(
        sck: Peri<'static, impl Pin>,
        io0: Peri<'static, impl Pin>,
        io1: Peri<'static, impl Pin>,
        io2: Peri<'static, impl Pin>,
        io3: Peri<'static, impl Pin>,
    ) -> Self {
        // Configure pins as flexible I/O
        let mut sck = Flex::new(sck);
        sck.set_as_output();
        sck.set_low();

        let mut io0 = Flex::new(io0);
        io0.set_as_output();
        io0.set_low();

        let mut io1 = Flex::new(io1);
        io1.set_as_input();
        io1.set_pull(Pull::None);

        let mut io2 = Flex::new(io2);
        io2.set_as_input();
        io2.set_pull(Pull::Up); // WP# should be high when not in multi-IO mode

        let mut io3 = Flex::new(io3);
        io3.set_as_input();
        io3.set_pull(Pull::Up); // HOLD# should be high when not in multi-IO mode

        Self {
            sck,
            io0,
            io1,
            io2,
            io3,
            clock_div: 125, // Default ~500 kHz (125 MHz / 2 / 125)
        }
    }

    /// Set the clock divider
    ///
    /// The actual SPI clock frequency is approximately:
    /// sys_clk / (2 * divider)
    pub fn set_clock_divider(&mut self, div: u16) {
        self.clock_div = div.max(1);
    }

    /// Small delay for clock timing
    #[inline(always)]
    fn delay(&self) {
        // Each cortex_m::asm::nop() is ~8ns at 125 MHz
        // Use div as approximate cycle count
        for _ in 0..self.clock_div {
            cortex_m::asm::nop();
        }
    }

    /// Configure IO pins for single-line mode (1-1-1)
    fn configure_single(&mut self) {
        self.io0.set_as_output();
        self.io1.set_as_input();
        self.io2.set_as_input();
        self.io3.set_as_input();
    }

    /// Configure IO pins for dual output mode (read on 2 lines)
    fn configure_dual_in(&mut self) {
        self.io0.set_as_input();
        self.io1.set_as_input();
        self.io2.set_as_input();
        self.io3.set_as_input();
    }

    /// Configure IO pins for dual output mode (write on 2 lines)
    fn configure_dual_out(&mut self) {
        self.io0.set_as_output();
        self.io1.set_as_output();
        self.io2.set_as_input();
        self.io3.set_as_input();
    }

    /// Configure IO pins for quad input mode (read on 4 lines)
    fn configure_quad_in(&mut self) {
        self.io0.set_as_input();
        self.io1.set_as_input();
        self.io2.set_as_input();
        self.io3.set_as_input();
    }

    /// Configure IO pins for quad output mode (write on 4 lines)
    fn configure_quad_out(&mut self) {
        self.io0.set_as_output();
        self.io1.set_as_output();
        self.io2.set_as_output();
        self.io3.set_as_output();
    }

    /// Write a single byte in the specified I/O mode
    pub fn write_byte(&mut self, byte: u8, mode: IoMode) {
        match mode {
            IoMode::Single => self.write_byte_single(byte),
            IoMode::DualOut | IoMode::DualIo => self.write_byte_dual(byte),
            IoMode::QuadOut | IoMode::QuadIo | IoMode::Qpi => self.write_byte_quad(byte),
        }
    }

    /// Read a single byte in the specified I/O mode
    pub fn read_byte(&mut self, mode: IoMode) -> u8 {
        match mode {
            IoMode::Single => self.read_byte_single(),
            IoMode::DualOut | IoMode::DualIo => self.read_byte_dual(),
            IoMode::QuadOut | IoMode::QuadIo | IoMode::Qpi => self.read_byte_quad(),
        }
    }

    /// Write a byte using single-line SPI (MSB first)
    fn write_byte_single(&mut self, byte: u8) {
        self.configure_single();

        for i in (0..8).rev() {
            // Set MOSI
            if (byte >> i) & 1 != 0 {
                self.io0.set_high();
            } else {
                self.io0.set_low();
            }

            self.delay();

            // Rising edge
            self.sck.set_high();
            self.delay();

            // Falling edge
            self.sck.set_low();
        }
    }

    /// Read a byte using single-line SPI (MSB first)
    fn read_byte_single(&mut self) -> u8 {
        self.configure_single();

        let mut byte = 0u8;

        for i in (0..8).rev() {
            // Output dummy high on MOSI during read
            self.io0.set_high();

            self.delay();

            // Rising edge - sample MISO
            self.sck.set_high();

            if self.io1.is_high() {
                byte |= 1 << i;
            }

            self.delay();

            // Falling edge
            self.sck.set_low();
        }

        byte
    }

    /// Write a byte using dual-line SPI (MSB first, 4 clocks)
    fn write_byte_dual(&mut self, byte: u8) {
        self.configure_dual_out();

        // 4 clock cycles, 2 bits per clock
        for i in (0..4).rev() {
            let nibble = (byte >> (i * 2)) & 0x03;

            // Set IO0 and IO1
            if nibble & 0x02 != 0 {
                self.io0.set_high();
            } else {
                self.io0.set_low();
            }
            if nibble & 0x01 != 0 {
                self.io1.set_high();
            } else {
                self.io1.set_low();
            }

            self.delay();

            // Rising edge
            self.sck.set_high();
            self.delay();

            // Falling edge
            self.sck.set_low();
        }
    }

    /// Read a byte using dual-line SPI (MSB first, 4 clocks)
    fn read_byte_dual(&mut self) -> u8 {
        self.configure_dual_in();

        let mut byte = 0u8;

        // 4 clock cycles, 2 bits per clock
        for i in (0..4).rev() {
            self.delay();

            // Rising edge - sample both lines
            self.sck.set_high();

            let mut nibble = 0u8;
            if self.io0.is_high() {
                nibble |= 0x02;
            }
            if self.io1.is_high() {
                nibble |= 0x01;
            }
            byte |= nibble << (i * 2);

            self.delay();

            // Falling edge
            self.sck.set_low();
        }

        byte
    }

    /// Write a byte using quad-line SPI (MSB first, 2 clocks)
    fn write_byte_quad(&mut self, byte: u8) {
        self.configure_quad_out();

        // 2 clock cycles, 4 bits per clock
        for i in (0..2).rev() {
            let nibble = (byte >> (i * 4)) & 0x0F;

            // Set IO0-IO3
            if nibble & 0x08 != 0 {
                self.io0.set_high();
            } else {
                self.io0.set_low();
            }
            if nibble & 0x04 != 0 {
                self.io1.set_high();
            } else {
                self.io1.set_low();
            }
            if nibble & 0x02 != 0 {
                self.io2.set_high();
            } else {
                self.io2.set_low();
            }
            if nibble & 0x01 != 0 {
                self.io3.set_high();
            } else {
                self.io3.set_low();
            }

            self.delay();

            // Rising edge
            self.sck.set_high();
            self.delay();

            // Falling edge
            self.sck.set_low();
        }
    }

    /// Read a byte using quad-line SPI (MSB first, 2 clocks)
    fn read_byte_quad(&mut self) -> u8 {
        self.configure_quad_in();

        let mut byte = 0u8;

        // 2 clock cycles, 4 bits per clock
        for i in (0..2).rev() {
            self.delay();

            // Rising edge - sample all 4 lines
            self.sck.set_high();

            let mut nibble = 0u8;
            if self.io0.is_high() {
                nibble |= 0x08;
            }
            if self.io1.is_high() {
                nibble |= 0x04;
            }
            if self.io2.is_high() {
                nibble |= 0x02;
            }
            if self.io3.is_high() {
                nibble |= 0x01;
            }
            byte |= nibble << (i * 4);

            self.delay();

            // Falling edge
            self.sck.set_low();
        }

        byte
    }

    /// Write multiple bytes
    pub fn write_bytes(&mut self, data: &[u8], mode: IoMode) {
        for &byte in data {
            self.write_byte(byte, mode);
        }
    }

    /// Read multiple bytes
    pub fn read_bytes(&mut self, buf: &mut [u8], mode: IoMode) {
        for byte in buf.iter_mut() {
            *byte = self.read_byte(mode);
        }
    }

    /// Full duplex transfer (single-line mode only)
    pub fn transfer(&mut self, write: &[u8], read: &mut [u8]) {
        self.configure_single();

        let max_len = write.len().max(read.len());

        for i in 0..max_len {
            let tx = if i < write.len() { write[i] } else { 0xFF };
            let mut rx = 0u8;

            for bit in (0..8).rev() {
                // Set MOSI
                if (tx >> bit) & 1 != 0 {
                    self.io0.set_high();
                } else {
                    self.io0.set_low();
                }

                self.delay();

                // Rising edge - sample MISO
                self.sck.set_high();

                if self.io1.is_high() {
                    rx |= 1 << bit;
                }

                self.delay();

                // Falling edge
                self.sck.set_low();
            }

            if i < read.len() {
                read[i] = rx;
            }
        }
    }
}
