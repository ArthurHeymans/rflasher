//! FTDI MPSSE device implementation using rs-ftdi (shared by native and wasm)
//!
//! This module provides the `Ftdi` struct using the pure-Rust `rs-ftdi` crate
//! (backed by `nusb`). It uses `maybe_async` to support both native sync and
//! WASM async modes from a single codebase.
//!
//! rs-ftdi handles USB communication, modem status byte stripping, and endpoint
//! management. This module layers MPSSE SPI protocol on top.

#[cfg(feature = "is_sync")]
use std::time::Duration;

use maybe_async::maybe_async;
#[cfg(feature = "is_sync")]
use nusb::MaybeFuture;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};
use rs_ftdi::FtdiDevice;

use crate::protocol::*;
use crate::rsftdi_error::{FtdiError, Result};

/// FTDI MPSSE programmer (rs-ftdi backend, shared by native and wasm)
///
/// This struct represents a connection to an FTDI device using the MPSSE
/// engine for SPI communication. It uses the pure-Rust `rs-ftdi` crate
/// and supports both native sync and WASM async modes via `maybe_async`.
pub struct Ftdi {
    /// rs-ftdi device context
    device: FtdiDevice,
    /// Current CS bits state
    cs_bits: u8,
    /// Auxiliary bits
    aux_bits: u8,
    /// Pin direction
    pindir: u8,
}

// ---------------------------------------------------------------------------
// Helper: convert our FtdiInterface to rs-ftdi's Interface
// ---------------------------------------------------------------------------

fn map_interface(iface: FtdiInterface) -> rs_ftdi::Interface {
    match iface {
        FtdiInterface::A => rs_ftdi::Interface::A,
        FtdiInterface::B => rs_ftdi::Interface::B,
        FtdiInterface::C => rs_ftdi::Interface::C,
        FtdiInterface::D => rs_ftdi::Interface::D,
    }
}

// ---------------------------------------------------------------------------
// Native-only methods (device enumeration, sync open, Drop)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "native", not(feature = "wasm")))]
impl Ftdi {
    /// Open an FTDI device with the given configuration
    pub fn open(config: &FtdiConfig) -> Result<Self> {
        log::info!(
            "Opening FTDI {} channel {} (rs-ftdi backend)",
            config.device_type.name(),
            config.interface.letter()
        );

        let interface = map_interface(config.interface);
        let vid = config.device_type.vendor_id();
        let pid = config.device_type.product_id();

        log::debug!("Looking for FTDI device VID={:04X} PID={:04X}", vid, pid);

        let mut device = FtdiDevice::open_with_interface(vid, pid, interface)
            .map_err(|e| FtdiError::OpenFailed(format!("{}", e)))?;

        log::debug!("Opened FTDI device VID={:04X} PID={:04X}", vid, pid);

        // Reset USB device
        device
            .usb_reset()
            .map_err(|e| FtdiError::ConfigFailed(format!("USB reset failed: {}", e)))?;

        // Set latency timer (2ms for best performance)
        device
            .set_latency_timer(2)
            .map_err(|e| FtdiError::ConfigFailed(format!("Set latency timer failed: {}", e)))?;

        // Set MPSSE bitbang mode
        device
            .set_bitmode(0x00, rs_ftdi::BitMode::Mpsse)
            .map_err(|e| FtdiError::ConfigFailed(format!("Set MPSSE mode failed: {}", e)))?;

        let mut ftdi = Ftdi {
            device,
            cs_bits: config.cs_bits,
            aux_bits: config.aux_bits,
            pindir: config.pindir,
        };

        // Initialize MPSSE
        ftdi.init_mpsse(config)?;

        log::info!(
            "FTDI configured for SPI at {:.2} MHz (rs-ftdi backend)",
            config.spi_clock_mhz()
        );

        Ok(ftdi)
    }

    /// Open the first available FTDI device
    pub fn open_first() -> Result<Self> {
        Self::open(&FtdiConfig::default())
    }

    /// Open a specific device type
    pub fn open_device(device_type: FtdiDeviceType) -> Result<Self> {
        Self::open(&FtdiConfig::for_device(device_type))
    }

    /// List available FTDI devices
    pub fn list_devices() -> Result<Vec<FtdiDeviceInfo>> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| FtdiError::UsbError(e.to_string()))?
            .filter_map(|dev| {
                let vid = dev.vendor_id();
                let pid = dev.product_id();

                get_device_info(vid, pid).map(|info| FtdiDeviceInfo {
                    bus: dev.busnum(),
                    address: dev.device_address(),
                    vendor_id: vid,
                    product_id: pid,
                    vendor_name: info.vendor_name,
                    device_name: info.device_name,
                    serial: None,
                })
            })
            .collect();

        Ok(devices)
    }
}

// Drop implementation only for sync mode (async requires explicit shutdown)
#[cfg(feature = "is_sync")]
impl Drop for Ftdi {
    fn drop(&mut self) {
        // Release I/O pins on close
        if let Err(e) = self.release_pins() {
            log::warn!("Failed to release pins on close: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, async open, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", not(feature = "is_sync")))]
impl Ftdi {
    /// Request an FTDI device via the WebUSB permission prompt
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    /// It shows the browser's device picker filtered to all supported FTDI devices.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        // Delegate to rs-ftdi's WebUSB device picker
        rs_ftdi::FtdiDevice::request_device()
            .await
            .map_err(|e| FtdiError::OpenFailed(format!("WebUSB request failed: {}", e)))
    }

    /// Open an FTDI device from a DeviceInfo with the given configuration
    pub async fn open(device_info: nusb::DeviceInfo, config: &FtdiConfig) -> Result<Self> {
        log::info!(
            "Opening FTDI {} channel {} VID={:04X} PID={:04X} (rs-ftdi WebUSB)",
            config.device_type.name(),
            config.interface.letter(),
            device_info.vendor_id(),
            device_info.product_id()
        );

        let interface = map_interface(config.interface);

        let mut device = FtdiDevice::open_wasm(device_info, interface)
            .await
            .map_err(|e| FtdiError::OpenFailed(format!("{}", e)))?;

        // Reset USB device
        device
            .usb_reset()
            .await
            .map_err(|e| FtdiError::ConfigFailed(format!("USB reset failed: {}", e)))?;

        // Set latency timer (2ms for best performance)
        device
            .set_latency_timer(2)
            .await
            .map_err(|e| FtdiError::ConfigFailed(format!("Set latency timer failed: {}", e)))?;

        // Set MPSSE bitbang mode
        device
            .set_bitmode(0x00, rs_ftdi::BitMode::Mpsse)
            .await
            .map_err(|e| FtdiError::ConfigFailed(format!("Set MPSSE mode failed: {}", e)))?;

        let mut ftdi = Ftdi {
            device,
            cs_bits: config.cs_bits,
            aux_bits: config.aux_bits,
            pindir: config.pindir,
        };

        // Initialize MPSSE
        ftdi.init_mpsse(config).await?;

        log::info!(
            "FTDI configured for SPI at {:.2} MHz (rs-ftdi WebUSB)",
            config.spi_clock_mhz()
        );

        Ok(ftdi)
    }

    /// Shutdown: release pins (WASM equivalent of Drop)
    pub async fn shutdown(&mut self) {
        if let Err(e) = self.release_pins().await {
            log::warn!("Failed to release pins on shutdown: {}", e);
        }
        self.device.shutdown().await;
        log::info!("FTDI shutdown complete");
    }
}

// ---------------------------------------------------------------------------
// Shared methods (sync or async via maybe_async)
// ---------------------------------------------------------------------------

impl Ftdi {
    /// Initialize the MPSSE engine with MPSSE commands
    #[maybe_async]
    async fn init_mpsse(&mut self, config: &FtdiConfig) -> Result<()> {
        let mut buf = Vec::with_capacity(32);

        // Disable divide-by-5 prescaler for 60 MHz base clock (H devices)
        if config.device_type.is_high_speed() {
            log::debug!("Disabling divide-by-5 prescaler for 60 MHz clock");
            buf.push(DIS_DIV_5);
        }

        // Set clock divisor
        // Divisor value for MPSSE is (divisor / 2 - 1)
        let divisor_val = config.divisor / 2 - 1;
        log::debug!(
            "Setting clock divisor to {} (SPI clock: {:.2} MHz)",
            config.divisor,
            config.spi_clock_mhz()
        );
        buf.push(TCK_DIVISOR);
        buf.push((divisor_val & 0xFF) as u8);
        buf.push(((divisor_val >> 8) & 0xFF) as u8);

        // Disconnect loopback
        log::debug!("Disabling loopback");
        buf.push(LOOPBACK_END);

        // Set initial data bits (low byte)
        log::debug!(
            "Setting data bits: cs_bits=0x{:02X} aux_bits=0x{:02X} pindir=0x{:02X}",
            config.cs_bits,
            config.aux_bits,
            config.pindir
        );
        buf.push(SET_BITS_LOW);
        buf.push(config.cs_bits | config.aux_bits);
        buf.push(config.pindir);

        // Set high byte pins if needed
        if config.pindir_high != 0 {
            log::debug!(
                "Setting high byte pins: aux_bits_high=0x{:02X} pindir_high=0x{:02X}",
                config.aux_bits_high,
                config.pindir_high
            );
            buf.push(SET_BITS_HIGH);
            buf.push(config.aux_bits_high);
            buf.push(config.pindir_high);
        }

        self.send(&buf).await?;

        Ok(())
    }

    /// Send data to the FTDI device
    #[maybe_async]
    async fn send(&mut self, data: &[u8]) -> Result<()> {
        self.device
            .write_all(data)
            .await
            .map_err(|e| FtdiError::TransferFailed(format!("Write failed: {}", e)))?;
        log::trace!("Sent {} bytes", data.len());
        Ok(())
    }

    /// Receive data from the FTDI device
    #[maybe_async]
    async fn recv(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let mut total = 0;

        while total < len {
            match self.device.read_data(&mut buf[total..]).await {
                Ok(0) => {
                    // No data available, wait a bit
                    #[cfg(feature = "is_sync")]
                    {
                        std::thread::sleep(Duration::from_micros(100));
                    }
                    #[cfg(all(feature = "wasm", not(feature = "is_sync")))]
                    {
                        let promise = js_sys::Promise::new(&mut |resolve, _| {
                            let window = web_sys::window().unwrap();
                            window
                                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 1)
                                .unwrap();
                        });
                        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                    }
                }
                Ok(n) => {
                    total += n;
                }
                Err(e) => {
                    return Err(FtdiError::TransferFailed(format!("Read failed: {}", e)));
                }
            }
        }

        log::trace!("Received {} bytes", total);
        Ok(buf)
    }

    /// Perform an SPI transfer via MPSSE commands
    #[maybe_async]
    async fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let writecnt = write_data.len();
        let readcnt = read_len;

        // Validate lengths
        if writecnt > 65536 || readcnt > 65536 {
            return Err(FtdiError::TransferFailed(
                "Transfer length exceeds 64KB limit".to_string(),
            ));
        }

        // Build MPSSE command buffer
        let mut buf = Vec::with_capacity(FTDI_HW_BUFFER_SIZE);

        // Assert CS
        buf.push(SET_BITS_LOW);
        buf.push(self.aux_bits);
        buf.push(self.pindir);

        // Write command (opcode + address + data)
        if writecnt > 0 {
            buf.push(MPSSE_DO_WRITE | MPSSE_WRITE_NEG);
            buf.push(((writecnt - 1) & 0xFF) as u8);
            buf.push((((writecnt - 1) >> 8) & 0xFF) as u8);
            buf.extend_from_slice(write_data);
        }

        // Read command
        if readcnt > 0 {
            buf.push(MPSSE_DO_READ);
            buf.push(((readcnt - 1) & 0xFF) as u8);
            buf.push((((readcnt - 1) >> 8) & 0xFF) as u8);
        }

        // Deassert CS
        buf.push(SET_BITS_LOW);
        buf.push(self.cs_bits | self.aux_bits);
        buf.push(self.pindir);

        // Send immediate to flush
        buf.push(SEND_IMMEDIATE);

        // Send command
        self.send(&buf).await?;

        // Read response if needed
        if readcnt > 0 {
            self.recv(readcnt).await
        } else {
            Ok(Vec::new())
        }
    }

    /// Release I/O pins (set all as inputs)
    #[maybe_async]
    async fn release_pins(&mut self) -> Result<()> {
        let buf = [SET_BITS_LOW, 0x00, 0x00];
        self.send(&buf).await
    }
}

// ---------------------------------------------------------------------------
// SpiMaster trait implementation
// ---------------------------------------------------------------------------

#[maybe_async(AFIT)]
impl SpiMaster for Ftdi {
    fn features(&self) -> SpiFeatures {
        // FTDI MPSSE supports 4-byte addressing (software handled)
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // FTDI can handle 64KB per transfer, but we chunk for responsiveness
        64 * 1024
    }

    fn max_write_len(&self) -> usize {
        // Page program is typically 256 bytes
        256
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check that the requested I/O mode is supported
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // Build the command bytes to send
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        // Encode opcode + address + dummy bytes
        cmd.encode_header(&mut write_data);

        // Append write data (for write commands)
        write_data[header_len..].copy_from_slice(cmd.write_data);

        // Perform the transfer
        let read_len = cmd.read_buf.len();
        let result = self
            .spi_transfer(&write_data, read_len)
            .await
            .map_err(|_e| CoreError::ProgrammerError)?;

        // Copy read data back
        cmd.read_buf.copy_from_slice(&result);

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        if us > 0 {
            #[cfg(feature = "is_sync")]
            {
                std::thread::sleep(Duration::from_micros(us as u64));
            }

            #[cfg(all(feature = "wasm", not(feature = "is_sync")))]
            {
                let delay_ms = ((us as f64) / 1000.0).ceil() as i32;
                if delay_ms > 0 {
                    let promise = js_sys::Promise::new(&mut |resolve, _| {
                        let window = web_sys::window().unwrap();
                        window
                            .set_timeout_with_callback_and_timeout_and_arguments_0(
                                &resolve, delay_ms,
                            )
                            .unwrap();
                    });
                    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Device info (for list_devices, native only)
// ---------------------------------------------------------------------------

/// Information about a connected FTDI device
#[cfg(all(feature = "native", not(feature = "wasm")))]
#[derive(Debug, Clone)]
pub struct FtdiDeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
    /// Vendor ID
    pub vendor_id: u16,
    /// Product ID
    pub product_id: u16,
    /// Vendor name
    pub vendor_name: &'static str,
    /// Device name
    pub device_name: &'static str,
    /// Serial number (if available)
    pub serial: Option<String>,
}

#[cfg(all(feature = "native", not(feature = "wasm")))]
impl std::fmt::Display for FtdiDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} at bus {} address {} ({:04X}:{:04X})",
            self.vendor_name,
            self.device_name,
            self.bus,
            self.address,
            self.vendor_id,
            self.product_id
        )
    }
}

// ---------------------------------------------------------------------------
// Option parsing (native only, not wasm)
// ---------------------------------------------------------------------------

/// Parse programmer options from a string
///
/// Format: "type=<type>,port=<A|B|C|D>,divisor=<N>,serial=<serial>,gpiol0=<H|L|C>"
#[cfg(all(feature = "native", not(feature = "wasm")))]
pub fn parse_options(options: &[(&str, &str)]) -> Result<FtdiConfig> {
    let mut config = FtdiConfig::default();

    for (key, value) in options {
        match *key {
            "type" => {
                config.device_type = FtdiDeviceType::parse(value).ok_or_else(|| {
                    FtdiError::InvalidDeviceType(format!(
                        "Unknown device type '{}'. Valid types: 2232h, 4232h, 232h, 4233h, \
                         jtagkey, tumpa, tumpalite, picotap, busblaster, flyswatter, \
                         arm-usb-ocd, arm-usb-tiny, arm-usb-ocd-h, arm-usb-tiny-h, \
                         google-servo, google-servo-v2, kt-link",
                        value
                    ))
                })?;
                // Update defaults for new device type
                config.cs_bits = config.device_type.default_cs_bits();
                config.aux_bits = config.device_type.default_aux_bits();
                config.pindir = config.device_type.default_pindir();
                config.pindir_high = config.device_type.default_pindir_high();
                config.divisor = config.device_type.default_divisor();
            }
            "port" | "channel" => {
                if value.len() != 1 {
                    return Err(FtdiError::InvalidChannel(format!(
                        "Invalid channel '{}': must be A, B, C, or D",
                        value
                    )));
                }
                let interface = FtdiInterface::from_char(value.chars().next().unwrap())
                    .ok_or_else(|| {
                        FtdiError::InvalidChannel(format!(
                            "Invalid channel '{}': must be A, B, C, or D",
                            value
                        ))
                    })?;
                config = config.interface(interface)?;
            }
            "divisor" => {
                let divisor: u16 = value.parse().map_err(|_| {
                    FtdiError::InvalidParameter(format!("Invalid divisor '{}'", value))
                })?;
                config = config.divisor(divisor)?;
            }
            "serial" => {
                config.serial = Some(value.to_string());
            }
            "description" => {
                config.description = Some(value.to_string());
            }
            key if key.starts_with("gpiol") => {
                let pin: u8 = key[5..].parse().map_err(|_| {
                    FtdiError::InvalidParameter(format!("Invalid GPIOL pin '{}'", key))
                })?;
                if value.len() != 1 {
                    return Err(FtdiError::InvalidParameter(format!(
                        "Invalid GPIOL mode '{}': must be H, L, C, or I",
                        value
                    )));
                }
                config = config.gpiol(pin, value.chars().next().unwrap())?;
            }
            _ => {
                log::warn!("Unknown FTDI option: {}={}", key, value);
            }
        }
    }

    Ok(config)
}
