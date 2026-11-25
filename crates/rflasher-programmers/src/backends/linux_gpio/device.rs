//! Linux GPIO SPI bitbanging device implementation
//!
//! This module provides the `LinuxGpioSpi` struct that implements the `SpiMaster`
//! trait using Linux's GPIO character device interface (gpiocdev).
//!
//! SPI is implemented via bit-banging, where GPIO pins are directly controlled
//! to generate SPI clock, data, and chip select signals.
//!
//! ## Multi-IO Support
//!
//! This programmer supports dual I/O (2-bit) and quad I/O (4-bit) modes when
//! the appropriate GPIO pins are configured:
//!
//! - **Single I/O (1-1-1)**: Uses MOSI for output and MISO for input
//! - **Dual I/O**: Uses IO0 (MOSI) and IO1 (MISO) bidirectionally
//! - **Quad I/O**: Uses IO0-IO3 bidirectionally (requires io2 and io3 pins)

use super::error::{LinuxGpioError, Result};

use gpiocdev::line::{Offset, Value};
use gpiocdev::request::{Config, Request};

use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::bitbang::{self, BitbangDualIo, BitbangQuadIo, BitbangSpiMaster};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{IoMode, SpiCommand, check_io_mode_supported};

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

/// Current I/O direction state for SPI data lines (MOSI/IO0, MISO/IO1, and optionally IO2/IO3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IoDirection {
    /// Single-I/O mode: MOSI=output, MISO/(IO2/IO3)=input (matches `open()` hardware config).
    Single,
    /// Multi-I/O write phase: all data lines configured as outputs.
    MultiOutput,
    /// Multi-I/O read phase: all data lines configured as inputs.
    MultiInput,
}

impl IoDirection {
    /// Transition to `target`, invoking `apply` only on state change and updating
    /// `self` only when `apply` succeeds.
    fn transition<E>(
        &mut self,
        target: IoDirection,
        apply: impl FnOnce(IoDirection) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        if *self == target {
            return Ok(());
        }
        apply(target)?;
        *self = target;
        Ok(())
    }
}

/// Linux GPIO SPI programmer using bitbanging
///
/// This struct implements the `SpiMaster` trait for Linux systems using
/// GPIO pins controlled via the gpiocdev crate (character device interface).
///
/// It also implements `BitbangSpiMaster`, `BitbangDualIo`, and optionally
/// `BitbangQuadIo` for multi-IO support.
pub struct LinuxGpioSpi {
    /// GPIO line request handle for control lines (CS, SCK).
    ///
    /// Kept separate from `data_request` because `gpiocdev::Request::reconfigure`
    /// reapplies the saved output values of all lines in the request, which would
    /// otherwise deassert CS during mid-transfer multi-I/O direction switches.
    ctrl_request: Request,
    /// GPIO line request handle for data lines (MOSI/IO0, MISO/IO1, optional IO2/IO3)
    data_request: Request,
    /// GPIO line offsets indexed by Line enum
    offsets: [Offset; MAX_LINES],
    /// Number of I/O lines (2 for single/dual, 4 for quad)
    io_lines: usize,
    /// Half-period delay in nanoseconds
    half_period_ns: u64,
    /// Current direction of SPI data lines
    io_direction: IoDirection,
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

        // Request control lines (CS, SCK) separately so data-line reconfiguration
        // cannot overwrite CS/SCK output values mid-transfer.
        // Initial state: CS=1 (high/inactive), SCK=0 (low)
        let mut ctrl_config = Config::default();
        ctrl_config.with_line(config.cs).as_output(Value::Active);
        ctrl_config.with_line(config.sck).as_output(Value::Inactive);

        let ctrl_request = Request::from_config(ctrl_config)
            .on_chip(&config.device)
            .with_consumer("rflasher")
            .request()
            .map_err(LinuxGpioError::LineRequestFailed)?;

        // Request data lines: initial state MOSI=output low, MISO/(IO2/IO3)=input
        let mut data_config = Config::default();
        data_config
            .with_line(config.mosi)
            .as_output(Value::Inactive);
        data_config.with_line(config.miso).as_input();
        if io_lines == 4 {
            data_config.with_line(config.io2.unwrap()).as_input();
            data_config.with_line(config.io3.unwrap()).as_input();
        }

        let data_request = Request::from_config(data_config)
            .on_chip(&config.device)
            .with_consumer("rflasher")
            .request()
            .map_err(LinuxGpioError::LineRequestFailed)?;

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
            ctrl_request,
            data_request,
            offsets,
            io_lines,
            half_period_ns: config.half_period_ns,
            io_direction: IoDirection::Single,
        })
    }

    /// Perform an SPI transaction (single I/O mode)
    fn spi_transaction(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> CoreResult<()> {
        self.configure_io(IoDirection::Single)?;

        // Assert CS (active low)
        BitbangSpiMaster::set_cs(self, true);

        // Write phase
        bitbang::single::write_bytes(self, write_data);

        // Read phase
        bitbang::single::read_bytes(self, read_buf);

        // De-assert CS
        self.deassert_cs();
        Ok(())
    }

    /// De-assert CS with SCK low and half-period setup/hold delays.
    fn deassert_cs(&mut self) {
        BitbangSpiMaster::set_sck(self, false);
        BitbangSpiMaster::half_period_delay(self);
        BitbangSpiMaster::set_cs(self, false);
        BitbangSpiMaster::half_period_delay(self);
    }

    /// Build a `gpiocdev` line reconfiguration matching `target`.
    fn direction_config(&self, target: IoDirection) -> Config {
        let mut cfg = Config::default();
        match target {
            IoDirection::Single => {
                cfg.with_line(self.offsets[Line::Mosi as usize])
                    .as_output(Value::Inactive);
                cfg.with_line(self.offsets[Line::Miso as usize]).as_input();
                if self.io_lines == 4 {
                    cfg.with_line(self.offsets[Line::Io2 as usize]).as_input();
                    cfg.with_line(self.offsets[Line::Io3 as usize]).as_input();
                }
            }
            IoDirection::MultiOutput => {
                cfg.with_line(self.offsets[Line::Mosi as usize])
                    .as_output(Value::Inactive);
                cfg.with_line(self.offsets[Line::Miso as usize])
                    .as_output(Value::Inactive);
                if self.io_lines == 4 {
                    cfg.with_line(self.offsets[Line::Io2 as usize])
                        .as_output(Value::Inactive);
                    cfg.with_line(self.offsets[Line::Io3 as usize])
                        .as_output(Value::Inactive);
                }
            }
            IoDirection::MultiInput => {
                cfg.with_line(self.offsets[Line::Mosi as usize]).as_input();
                cfg.with_line(self.offsets[Line::Miso as usize]).as_input();
                if self.io_lines == 4 {
                    cfg.with_line(self.offsets[Line::Io2 as usize]).as_input();
                    cfg.with_line(self.offsets[Line::Io3 as usize]).as_input();
                }
            }
        }
        cfg
    }

    /// Reconfigure the SPI data lines to `target`, leaving `io_direction`
    /// unchanged if the kernel rejects the reconfiguration.
    fn configure_io(&mut self, target: IoDirection) -> CoreResult<()> {
        let cfg = self.direction_config(target);
        let data_request = &self.data_request;
        self.io_direction.transition(target, |_| {
            data_request.reconfigure(&cfg).map_err(|e| {
                log::error!("Failed to configure I/O lines to {:?}: {}", target, e);
                CoreError::ProgrammerError
            })
        })
    }

    /// Set dual I/O lines (IO0/IO1) values
    ///
    /// `io` bits: bit 0 -> IO0, bit 1 -> IO1
    fn set_dual_io(&self, io: u8) {
        let io0 = if io & 0x1 != 0 {
            Value::Active
        } else {
            Value::Inactive
        };
        let io1 = if io & 0x2 != 0 {
            Value::Active
        } else {
            Value::Inactive
        };

        if let Err(e) = self
            .data_request
            .set_value(self.offsets[Line::Mosi as usize], io0)
        {
            log::error!("Failed to set IO0: {}", e);
        }
        if let Err(e) = self
            .data_request
            .set_value(self.offsets[Line::Miso as usize], io1)
        {
            log::error!("Failed to set IO1: {}", e);
        }
    }

    /// Get dual I/O lines (IO0/IO1) values
    ///
    /// Returns bits: bit 0 <- IO0, bit 1 <- IO1
    fn get_dual_io(&self) -> u8 {
        let mut result = 0u8;

        if let Ok(Value::Active) = self.data_request.value(self.offsets[Line::Mosi as usize]) {
            result |= 0x1;
        }
        if let Ok(Value::Active) = self.data_request.value(self.offsets[Line::Miso as usize]) {
            result |= 0x2;
        }

        result
    }

    /// Set quad I/O lines (IO0/IO1/IO2/IO3) values
    ///
    /// `io` bits: bit 0 -> IO0, bit 1 -> IO1, bit 2 -> IO2, bit 3 -> IO3
    fn set_quad_io(&self, io: u8) {
        let vals = [
            if io & 0x1 != 0 {
                Value::Active
            } else {
                Value::Inactive
            },
            if io & 0x2 != 0 {
                Value::Active
            } else {
                Value::Inactive
            },
            if io & 0x4 != 0 {
                Value::Active
            } else {
                Value::Inactive
            },
            if io & 0x8 != 0 {
                Value::Active
            } else {
                Value::Inactive
            },
        ];

        if let Err(e) = self
            .data_request
            .set_value(self.offsets[Line::Mosi as usize], vals[0])
        {
            log::error!("Failed to set IO0: {}", e);
        }
        if let Err(e) = self
            .data_request
            .set_value(self.offsets[Line::Miso as usize], vals[1])
        {
            log::error!("Failed to set IO1: {}", e);
        }
        if let Err(e) = self
            .data_request
            .set_value(self.offsets[Line::Io2 as usize], vals[2])
        {
            log::error!("Failed to set IO2: {}", e);
        }
        if let Err(e) = self
            .data_request
            .set_value(self.offsets[Line::Io3 as usize], vals[3])
        {
            log::error!("Failed to set IO3: {}", e);
        }
    }

    /// Get quad I/O lines (IO0/IO1/IO2/IO3) values
    ///
    /// Returns bits: bit 0 <- IO0, bit 1 <- IO1, bit 2 <- IO2, bit 3 <- IO3
    fn get_quad_io(&self) -> u8 {
        let mut result = 0u8;

        if let Ok(Value::Active) = self.data_request.value(self.offsets[Line::Mosi as usize]) {
            result |= 0x1;
        }
        if let Ok(Value::Active) = self.data_request.value(self.offsets[Line::Miso as usize]) {
            result |= 0x2;
        }
        if let Ok(Value::Active) = self.data_request.value(self.offsets[Line::Io2 as usize]) {
            result |= 0x4;
        }
        if let Ok(Value::Active) = self.data_request.value(self.offsets[Line::Io3 as usize]) {
            result |= 0x8;
        }

        result
    }

    /// Check if this programmer has quad I/O capability
    pub fn has_quad_io(&self) -> bool {
        self.io_lines == 4
    }

    /// Execute an SPI command with multi-IO support
    fn execute_multi_io(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // Build the header: opcode + address + dummy bytes
        let header_len = cmd.header_len();
        let mut header = vec![0u8; header_len];
        cmd.encode_header(&mut header);

        // Configure initial data-line directions before asserting CS so a failed
        // reconfigure does not leave the chip selected.
        let initial_dir = match cmd.io_mode {
            IoMode::Qpi => IoDirection::MultiOutput,
            _ => IoDirection::Single,
        };
        self.configure_io(initial_dir)?;

        self.set_cs_active(true);

        let result = (|| -> CoreResult<()> {
            match cmd.io_mode {
                IoMode::Single => {
                    bitbang::single::write_bytes(self, &header);
                    bitbang::single::write_bytes(self, cmd.write_data);
                    bitbang::single::read_bytes(self, cmd.read_buf);
                }
                IoMode::DualOut => {
                    // 1-1-2: Opcode+address single, data dual (read only)
                    bitbang::single::write_bytes(self, &header);
                    bitbang::single::write_bytes(self, cmd.write_data);
                    self.configure_io(IoDirection::MultiInput)?;
                    bitbang::dual::read_bytes(self, cmd.read_buf);
                }
                IoMode::DualIo => {
                    // 1-2-2: Opcode single, address+data dual
                    if !header.is_empty() {
                        bitbang::single::write_byte(self, header[0]); // opcode
                    }
                    self.configure_io(IoDirection::MultiOutput)?;
                    bitbang::dual::write_bytes(self, &header[1..]); // address + dummy
                    bitbang::dual::write_bytes(self, cmd.write_data);
                    self.configure_io(IoDirection::MultiInput)?;
                    bitbang::dual::read_bytes(self, cmd.read_buf);
                }
                IoMode::QuadOut => {
                    // 1-1-4: Opcode+address single, data quad (read only)
                    bitbang::single::write_bytes(self, &header);
                    bitbang::single::write_bytes(self, cmd.write_data);
                    self.configure_io(IoDirection::MultiInput)?;
                    bitbang::quad::read_bytes(self, cmd.read_buf);
                }
                IoMode::QuadIo => {
                    // 1-4-4: Opcode single, address+data quad
                    if !header.is_empty() {
                        bitbang::single::write_byte(self, header[0]); // opcode
                    }
                    self.configure_io(IoDirection::MultiOutput)?;
                    bitbang::quad::write_bytes(self, &header[1..]); // address + dummy
                    bitbang::quad::write_bytes(self, cmd.write_data);
                    self.configure_io(IoDirection::MultiInput)?;
                    bitbang::quad::read_bytes(self, cmd.read_buf);
                }
                IoMode::Qpi => {
                    // 4-4-4: Everything quad
                    bitbang::quad::write_bytes(self, &header);
                    bitbang::quad::write_bytes(self, cmd.write_data);
                    self.configure_io(IoDirection::MultiInput)?;
                    bitbang::quad::read_bytes(self, cmd.read_buf);
                }
            }
            Ok(())
        })();

        // Always deassert CS even if a mid-transaction reconfigure failed.
        self.deassert_cs();

        result
    }

    /// Set CS with clearer semantics (true = chip active)
    #[inline]
    fn set_cs_active(&mut self, active: bool) {
        BitbangSpiMaster::set_cs(self, active);
    }
}

// Implement BitbangSpiMaster trait
impl BitbangSpiMaster for LinuxGpioSpi {
    fn set_cs(&mut self, active: bool) {
        // CS is active low
        let value = if active {
            Value::Inactive
        } else {
            Value::Active
        };
        if let Err(e) = self
            .ctrl_request
            .set_value(self.offsets[Line::Cs as usize], value)
        {
            log::error!("Failed to set CS: {}", e);
        }
    }

    fn set_sck(&mut self, high: bool) {
        let value = if high { Value::Active } else { Value::Inactive };
        if let Err(e) = self
            .ctrl_request
            .set_value(self.offsets[Line::Sck as usize], value)
        {
            log::error!("Failed to set SCK: {}", e);
        }
    }

    fn set_mosi(&mut self, high: bool) {
        let value = if high { Value::Active } else { Value::Inactive };
        if let Err(e) = self
            .data_request
            .set_value(self.offsets[Line::Mosi as usize], value)
        {
            log::error!("Failed to set MOSI: {}", e);
        }
    }

    fn get_miso(&self) -> bool {
        match self.data_request.value(self.offsets[Line::Miso as usize]) {
            Ok(Value::Active) => true,
            Ok(Value::Inactive) => false,
            Err(e) => {
                log::error!("Failed to get MISO: {}", e);
                false
            }
        }
    }

    fn half_period_delay(&self) {
        if self.half_period_ns > 0 {
            std::thread::sleep(std::time::Duration::from_nanos(self.half_period_ns));
        }
    }
}

// Implement BitbangDualIo trait (always available since we have at least IO0/IO1)
impl BitbangDualIo for LinuxGpioSpi {
    fn set_sck_set_dual_io(&mut self, sck: bool, io: u8) {
        // Ensure we're in output mode
        let _ = self.configure_io(IoDirection::MultiOutput);

        BitbangSpiMaster::set_sck(self, sck);
        self.set_dual_io(io);
    }

    fn set_sck_get_dual_io(&mut self, sck: bool) -> u8 {
        BitbangSpiMaster::set_sck(self, sck);
        self.get_dual_io()
    }

    fn set_idle_io(&mut self) {
        let _ = self.configure_io(IoDirection::MultiInput);
    }
}

// Implement BitbangQuadIo trait (conditionally, only if we have IO2/IO3)
// Note: We implement it unconditionally but the quad methods will log warnings
// if called without quad pins configured
impl BitbangQuadIo for LinuxGpioSpi {
    fn set_sck_set_quad_io(&mut self, sck: bool, io: u8) {
        if self.io_lines != 4 {
            log::warn!("set_sck_set_quad_io called but quad I/O not configured");
            return;
        }

        // Ensure we're in output mode
        let _ = self.configure_io(IoDirection::MultiOutput);

        BitbangSpiMaster::set_sck(self, sck);
        self.set_quad_io(io);
    }

    fn set_sck_get_quad_io(&mut self, sck: bool) -> u8 {
        if self.io_lines != 4 {
            log::warn!("set_sck_get_quad_io called but quad I/O not configured");
            return 0;
        }

        BitbangSpiMaster::set_sck(self, sck);
        self.get_quad_io()
    }
}

impl SpiMaster for LinuxGpioSpi {
    fn features(&self) -> SpiFeatures {
        // Bitbang supports 4-byte addressing (done in software)
        // Dual I/O is always available (IO0/IO1 = MOSI/MISO)
        // Quad I/O only if io2/io3 pins are configured
        let mut features = SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::DUAL;
        if self.io_lines == 4 {
            features |= SpiFeatures::QUAD | SpiFeatures::QPI;
        }
        features
    }

    fn max_read_len(&self) -> usize {
        // No hardware limit for bitbanging
        usize::MAX
    }

    fn max_write_len(&self) -> usize {
        // No hardware limit for bitbanging
        usize::MAX
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Use multi-IO execution if the command requests it
        if cmd.io_mode != IoMode::Single {
            return self.execute_multi_io(cmd);
        }

        // Fast path for single I/O mode (most common case)
        // Build the write data: opcode + address + dummy + write_data
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        // Encode opcode + address + dummy bytes
        cmd.encode_header(&mut write_data);

        // Append write data
        write_data[header_len..].copy_from_slice(cmd.write_data);

        // Perform SPI transfer
        self.spi_transaction(&write_data, cmd.read_buf)
    }

    async fn delay_us(&mut self, us: u32) {
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
pub fn parse_options(options: &[(&str, &str)]) -> Result<LinuxGpioSpiConfig> {
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
                gpiochip = Some(value.parse().map_err(|_| {
                    LinuxGpioError::InvalidParameter(format!("Invalid gpiochip value: {}", value))
                })?);
            }
            "cs" => {
                config.cs = value
                    .parse()
                    .map_err(|_| LinuxGpioError::InvalidLineNumber {
                        name: "cs",
                        value: (*value).to_string(),
                    })?;
                have_cs = true;
            }
            "sck" => {
                config.sck = value
                    .parse()
                    .map_err(|_| LinuxGpioError::InvalidLineNumber {
                        name: "sck",
                        value: (*value).to_string(),
                    })?;
                have_sck = true;
            }
            "mosi" | "io0" => {
                config.mosi = value
                    .parse()
                    .map_err(|_| LinuxGpioError::InvalidLineNumber {
                        name: "mosi/io0",
                        value: (*value).to_string(),
                    })?;
                have_mosi = true;
            }
            "miso" | "io1" => {
                config.miso = value
                    .parse()
                    .map_err(|_| LinuxGpioError::InvalidLineNumber {
                        name: "miso/io1",
                        value: (*value).to_string(),
                    })?;
                have_miso = true;
            }
            "io2" => {
                config.io2 =
                    Some(
                        value
                            .parse()
                            .map_err(|_| LinuxGpioError::InvalidLineNumber {
                                name: "io2",
                                value: (*value).to_string(),
                            })?,
                    );
            }
            "io3" => {
                config.io3 =
                    Some(
                        value
                            .parse()
                            .map_err(|_| LinuxGpioError::InvalidLineNumber {
                                name: "io3",
                                value: (*value).to_string(),
                            })?,
                    );
            }
            "spispeed" => {
                let speed_khz = crate::catalog::parse_speed_khz(value).ok_or_else(|| {
                    LinuxGpioError::InvalidParameter(format!("Invalid spispeed value: {value}"))
                })?;
                config = config.with_speed_hz(speed_khz * 1000);
            }
            _ => {
                return Err(LinuxGpioError::InvalidParameter(format!(
                    "unknown option: {key}"
                )));
            }
        }
    }

    // Handle dev vs gpiochip
    if config.device.is_empty() {
        if let Some(n) = gpiochip {
            if n > 9 {
                return Err(LinuxGpioError::InvalidParameter(
                    "Maximum gpiochip number supported is 9".to_string(),
                ));
            }
            config.device = format!("/dev/gpiochip{}", n);
        } else {
            return Err(LinuxGpioError::NoDevice);
        }
    } else if gpiochip.is_some() {
        return Err(LinuxGpioError::InvalidParameter(
            "Only one of 'dev' or 'gpiochip' can be specified".to_string(),
        ));
    }

    // Check required parameters
    if !have_cs {
        return Err(LinuxGpioError::MissingParameter("cs"));
    }
    if !have_sck {
        return Err(LinuxGpioError::MissingParameter("sck"));
    }
    if !have_mosi {
        return Err(LinuxGpioError::MissingParameter("mosi (or io0)"));
    }
    if !have_miso {
        return Err(LinuxGpioError::MissingParameter("miso (or io1)"));
    }

    // Check quad I/O consistency
    if config.io2.is_some() != config.io3.is_some() {
        return Err(LinuxGpioError::IncompleteQuadIo);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_direction_transitions_track_hardware_state_and_preserve_state_on_error() {
        let mut dir = IoDirection::Single;
        let mut reconfigs = Vec::new();

        // Already in Single: no-op.
        dir.transition(IoDirection::Single, |target| {
            reconfigs.push(target);
            Ok::<(), ()>(())
        })
        .unwrap();
        assert!(reconfigs.is_empty());

        // Single -> MultiInput must actually reconfigure (MOSI changes from output to input).
        dir.transition(IoDirection::MultiInput, |target| {
            reconfigs.push(target);
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(dir, IoDirection::MultiInput);
        assert_eq!(reconfigs, vec![IoDirection::MultiInput]);

        // Failed transition back to Single must leave state at MultiInput so a retry runs.
        assert!(
            dir.transition(IoDirection::Single, |target| {
                reconfigs.push(target);
                Err::<(), _>(())
            })
            .is_err()
        );
        assert_eq!(dir, IoDirection::MultiInput);

        // Retry succeeds and restores Single mode.
        dir.transition(IoDirection::Single, |target| {
            reconfigs.push(target);
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(dir, IoDirection::Single);
        assert_eq!(
            reconfigs,
            vec![
                IoDirection::MultiInput,
                IoDirection::Single,
                IoDirection::Single
            ]
        );
    }
}
