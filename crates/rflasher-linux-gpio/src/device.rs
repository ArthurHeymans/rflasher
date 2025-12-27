//! Linux GPIO SPI bitbanging device implementation
//!
//! This module provides the `LinuxGpioSpi` struct that implements the `SpiMaster`
//! trait using Linux's GPIO character device interface (gpiocdev).
//!
//! SPI is implemented via bit-banging, where GPIO pins are directly controlled
//! to generate SPI clock, data, and chip select signals.

use crate::error::{LinuxGpioError, Result};

use gpiocdev::line::{Offset, Value};
use gpiocdev::request::{Config, Request};

use rflasher_core::error::Result as CoreResult;
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;

/// GPIO line indices
#[derive(Debug, Clone, Copy)]
enum Line {
    Cs = 0,
    Sck = 1,
    Mosi = 2, // Also IO0 in dual/quad mode
    Miso = 3, // Also IO1 in dual/quad mode
    Io2 = 4,  // Only for quad mode
    Io3 = 5,  // Only for quad mode
}

/// Maximum number of GPIO lines we use
const MAX_LINES: usize = 6;

/// Default half-period delay in nanoseconds (for ~100 kHz SPI clock)
const DEFAULT_HALF_PERIOD_NS: u64 = 5000;

/// Configuration for opening a Linux GPIO SPI device
#[derive(Debug, Clone)]
pub struct LinuxGpioSpiConfig {
    /// Device path (e.g., "/dev/gpiochip0")
    pub device: String,
    /// CS (Chip Select) GPIO line offset
    pub cs: Offset,
    /// SCK (Clock) GPIO line offset
    pub sck: Offset,
    /// MOSI (Master Out Slave In) / IO0 GPIO line offset
    pub mosi: Offset,
    /// MISO (Master In Slave Out) / IO1 GPIO line offset
    pub miso: Offset,
    /// IO2 GPIO line offset (for quad mode, optional)
    pub io2: Option<Offset>,
    /// IO3 GPIO line offset (for quad mode, optional)
    pub io3: Option<Offset>,
    /// Half-period delay in nanoseconds
    pub half_period_ns: u64,
}

impl Default for LinuxGpioSpiConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            cs: 0,
            sck: 0,
            mosi: 0,
            miso: 0,
            io2: None,
            io3: None,
            half_period_ns: DEFAULT_HALF_PERIOD_NS,
        }
    }
}

impl LinuxGpioSpiConfig {
    /// Create a new configuration with the given device path and required pins
    pub fn new(
        device: impl Into<String>,
        cs: Offset,
        sck: Offset,
        mosi: Offset,
        miso: Offset,
    ) -> Self {
        Self {
            device: device.into(),
            cs,
            sck,
            mosi,
            miso,
            ..Default::default()
        }
    }

    /// Set IO2 and IO3 pins for quad mode
    pub fn with_quad_io(mut self, io2: Offset, io3: Offset) -> Self {
        self.io2 = Some(io2);
        self.io3 = Some(io3);
        self
    }

    /// Set the half-period delay in nanoseconds
    pub fn with_half_period_ns(mut self, ns: u64) -> Self {
        self.half_period_ns = ns;
        self
    }

    /// Set SPI speed in Hz (approximate, via half-period calculation)
    pub fn with_speed_hz(mut self, hz: u32) -> Self {
        // half_period = 1 / (2 * frequency) in seconds
        // = 1_000_000_000 / (2 * frequency) in nanoseconds
        if hz > 0 {
            self.half_period_ns = 500_000_000 / hz as u64;
        }
        self
    }
}

/// Linux GPIO SPI programmer using bitbanging
///
/// This struct implements the `SpiMaster` trait for Linux systems using
/// GPIO pins controlled via the gpiocdev crate (character device interface).
pub struct LinuxGpioSpi {
    /// GPIO line request handle
    request: Request,
    /// GPIO line offsets indexed by Line enum
    offsets: [Offset; MAX_LINES],
    /// Number of I/O lines (2 for single/dual, 4 for quad)
    #[allow(dead_code)]
    io_lines: usize,
    /// Half-period delay in nanoseconds
    half_period_ns: u64,
}

impl LinuxGpioSpi {
    /// Open a Linux GPIO SPI device with the given configuration
    pub fn open(config: &LinuxGpioSpiConfig) -> Result<Self> {
        if config.device.is_empty() {
            return Err(LinuxGpioError::NoDevice);
        }

        // Validate quad I/O configuration
        if config.io2.is_some() != config.io3.is_some() {
            return Err(LinuxGpioError::IncompleteQuadIo);
        }

        log::debug!("linux_gpio_spi: Opening device {}", config.device);

        // Build line offsets array
        let mut offsets = [0u32; MAX_LINES];
        offsets[Line::Cs as usize] = config.cs;
        offsets[Line::Sck as usize] = config.sck;
        offsets[Line::Mosi as usize] = config.mosi;
        offsets[Line::Miso as usize] = config.miso;

        let io_lines = if let (Some(io2), Some(io3)) = (config.io2, config.io3) {
            offsets[Line::Io2 as usize] = io2;
            offsets[Line::Io3 as usize] = io3;
            4
        } else {
            2
        };

        // Build line request configuration using gpiocdev
        // Initial state: CS=1 (high/inactive), SCK=0 (low), MOSI=0, MISO=input
        let mut req_config = Config::default();

        // Configure output lines: CS, SCK, MOSI
        req_config.with_line(config.cs).as_output(Value::Active); // CS starts high (inactive)
        req_config.with_line(config.sck).as_output(Value::Inactive); // SCK starts low
        req_config.with_line(config.mosi).as_output(Value::Inactive); // MOSI starts low

        // Configure MISO as input
        req_config.with_line(config.miso).as_input();

        // Configure IO2 and IO3 as input if present
        if io_lines == 4 {
            req_config.with_line(config.io2.unwrap()).as_input();
            req_config.with_line(config.io3.unwrap()).as_input();
        }

        // Request the lines
        let request = Request::from_config(req_config)
            .on_chip(&config.device)
            .with_consumer("rflasher")
            .request()
            .map_err(|e| LinuxGpioError::LineRequestFailed(e))?;

        log::info!(
            "linux_gpio_spi: Opened {} (cs={}, sck={}, mosi={}, miso={}{})",
            config.device,
            config.cs,
            config.sck,
            config.mosi,
            config.miso,
            if io_lines == 4 {
                format!(", io2={}, io3={}", config.io2.unwrap(), config.io3.unwrap())
            } else {
                String::new()
            }
        );

        Ok(Self {
            request,
            offsets,
            io_lines,
            half_period_ns: config.half_period_ns,
        })
    }

    /// Delay for half a clock period
    #[inline]
    fn half_period_delay(&self) {
        if self.half_period_ns > 0 {
            std::thread::sleep(std::time::Duration::from_nanos(self.half_period_ns));
        }
    }

    /// Set chip select (CS is active low)
    #[inline]
    fn set_cs(&self, active: bool) {
        let value = if active {
            Value::Inactive
        } else {
            Value::Active
        };
        if let Err(e) = self
            .request
            .set_value(self.offsets[Line::Cs as usize], value)
        {
            log::error!("Failed to set CS: {}", e);
        }
    }

    /// Set clock line
    #[inline]
    fn set_sck(&self, high: bool) {
        let value = if high { Value::Active } else { Value::Inactive };
        if let Err(e) = self
            .request
            .set_value(self.offsets[Line::Sck as usize], value)
        {
            log::error!("Failed to set SCK: {}", e);
        }
    }

    /// Set MOSI line
    #[inline]
    fn set_mosi(&self, high: bool) {
        let value = if high { Value::Active } else { Value::Inactive };
        if let Err(e) = self
            .request
            .set_value(self.offsets[Line::Mosi as usize], value)
        {
            log::error!("Failed to set MOSI: {}", e);
        }
    }

    /// Get MISO line value
    #[inline]
    fn get_miso(&self) -> bool {
        match self.request.value(self.offsets[Line::Miso as usize]) {
            Ok(Value::Active) => true,
            Ok(Value::Inactive) => false,
            Err(e) => {
                log::error!("Failed to get MISO: {}", e);
                false
            }
        }
    }

    /// Write a byte to SPI (bit-bang, MSB first)
    fn write_byte(&self, byte: u8) {
        for i in (0..8).rev() {
            let bit = (byte >> i) & 1 != 0;
            self.set_sck(false);
            self.set_mosi(bit);
            self.half_period_delay();
            self.set_sck(true);
            self.half_period_delay();
        }
    }

    /// Read a byte from SPI (bit-bang, MSB first)
    fn read_byte(&self) -> u8 {
        let mut byte = 0u8;
        for _ in 0..8 {
            self.set_sck(false);
            self.half_period_delay();
            byte <<= 1;
            if self.get_miso() {
                byte |= 1;
            }
            self.set_sck(true);
            self.half_period_delay();
        }
        byte
    }

    /// Perform an SPI transaction
    fn spi_transaction(&self, write_data: &[u8], read_buf: &mut [u8]) {
        // Assert CS (active low)
        self.set_cs(true);

        // Write phase
        for &byte in write_data {
            self.write_byte(byte);
        }

        // Read phase
        for byte in read_buf.iter_mut() {
            *byte = self.read_byte();
        }

        // De-assert CS
        self.set_sck(false);
        self.half_period_delay();
        self.set_cs(false);
        self.half_period_delay();
    }
}

impl SpiMaster for LinuxGpioSpi {
    fn features(&self) -> SpiFeatures {
        // Bitbang supports 4-byte addressing (done in software)
        // Could add dual/quad support if io_lines == 4
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // No hardware limit for bitbanging
        usize::MAX
    }

    fn max_write_len(&self) -> usize {
        // No hardware limit for bitbanging
        usize::MAX
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Build the write data: opcode + address + dummy + write_data
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        // Encode opcode + address + dummy bytes
        cmd.encode_header(&mut write_data);

        // Append write data
        write_data[header_len..].copy_from_slice(cmd.write_data);

        // Perform SPI transfer
        self.spi_transaction(&write_data, cmd.read_buf);

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

/// Parse programmer options from a list of key-value pairs
///
/// # Supported Options
///
/// - `dev=/dev/gpiochipN` - GPIO chip device path (required, or use gpiochip)
/// - `gpiochip=N` - GPIO chip number (alternative to dev)
/// - `cs=N` - CS (chip select) GPIO line offset (required)
/// - `sck=N` - SCK (clock) GPIO line offset (required)
/// - `mosi=N` or `io0=N` - MOSI GPIO line offset (required)
/// - `miso=N` or `io1=N` - MISO GPIO line offset (required)
/// - `io2=N` - IO2 GPIO line offset (optional, for quad mode)
/// - `io3=N` - IO3 GPIO line offset (optional, for quad mode)
/// - `spispeed=N` - SPI speed in kHz (optional, default ~100 kHz)
pub fn parse_options(options: &[(&str, &str)]) -> std::result::Result<LinuxGpioSpiConfig, String> {
    let mut config = LinuxGpioSpiConfig::default();
    let mut have_cs = false;
    let mut have_sck = false;
    let mut have_mosi = false;
    let mut have_miso = false;
    let mut gpiochip: Option<u32> = None;

    for (key, value) in options {
        match *key {
            "dev" => {
                config.device = value.to_string();
            }
            "gpiochip" => {
                gpiochip = Some(
                    value
                        .parse()
                        .map_err(|_| format!("Invalid gpiochip value: {}", value))?,
                );
            }
            "cs" => {
                config.cs = value
                    .parse()
                    .map_err(|_| format!("Invalid cs value: {}", value))?;
                have_cs = true;
            }
            "sck" => {
                config.sck = value
                    .parse()
                    .map_err(|_| format!("Invalid sck value: {}", value))?;
                have_sck = true;
            }
            "mosi" | "io0" => {
                config.mosi = value
                    .parse()
                    .map_err(|_| format!("Invalid mosi/io0 value: {}", value))?;
                have_mosi = true;
            }
            "miso" | "io1" => {
                config.miso = value
                    .parse()
                    .map_err(|_| format!("Invalid miso/io1 value: {}", value))?;
                have_miso = true;
            }
            "io2" => {
                config.io2 = Some(
                    value
                        .parse()
                        .map_err(|_| format!("Invalid io2 value: {}", value))?,
                );
            }
            "io3" => {
                config.io3 = Some(
                    value
                        .parse()
                        .map_err(|_| format!("Invalid io3 value: {}", value))?,
                );
            }
            "spispeed" => {
                let speed_khz: u32 = value
                    .parse()
                    .map_err(|_| format!("Invalid spispeed value: {}", value))?;
                config = config.with_speed_hz(speed_khz * 1000);
            }
            _ => {
                log::warn!("linux_gpio_spi: Unknown option: {}={}", key, value);
            }
        }
    }

    // Handle dev vs gpiochip
    if config.device.is_empty() {
        if let Some(n) = gpiochip {
            if n > 9 {
                return Err("Maximum gpiochip number supported is 9".to_string());
            }
            config.device = format!("/dev/gpiochip{}", n);
        } else {
            return Err("Either 'dev' or 'gpiochip' must be specified.\n\
                 e.g. linux_gpio_spi:dev=/dev/gpiochip0,cs=25,sck=11,mosi=10,miso=9"
                .to_string());
        }
    } else if gpiochip.is_some() {
        return Err("Only one of 'dev' or 'gpiochip' can be specified".to_string());
    }

    // Check required parameters
    if !have_cs {
        return Err("Missing required parameter: cs".to_string());
    }
    if !have_sck {
        return Err("Missing required parameter: sck".to_string());
    }
    if !have_mosi {
        return Err("Missing required parameter: mosi (or io0)".to_string());
    }
    if !have_miso {
        return Err("Missing required parameter: miso (or io1)".to_string());
    }

    // Check quad I/O consistency
    if config.io2.is_some() != config.io3.is_some() {
        return Err("Both io2 and io3 must be specified for quad I/O mode".to_string());
    }

    Ok(config)
}
