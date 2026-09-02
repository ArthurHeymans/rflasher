//! FTDI MPSSE device implementation using ftdi-nusb (shared by native and wasm)
//!
//! This module provides the `Ftdi` struct using the pure-Rust `ftdi-nusb` crate
//! (backed by `nusb`). The API is async on every target.
//!
//! ftdi-nusb handles USB communication, MPSSE state, and SPI transactions. This
//! module only maps rflasher's flashrom-compatible configuration and traits.

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use ftdi_nusb::{
    FtdiDevice,
    mpsse::{
        MpsseContext,
        spi::{SpiConfig, SpiDevice, SpiMode},
    },
};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{SpiCommand, check_io_mode_supported};

use super::error::{FtdiError, Result};
use super::protocol::*;

/// FTDI MPSSE programmer (ftdi-nusb backend, shared by native and wasm)
///
/// This struct represents a connection to an FTDI device using the MPSSE
/// engine for SPI communication. It uses the pure-Rust `ftdi-nusb` crate.
pub struct Ftdi {
    /// ftdi-nusb device context
    device: FtdiDevice,
    /// MPSSE engine state paired with `device`
    mpsse: MpsseContext,
    /// Configured SPI bus and adapter pin state
    spi: SpiDevice,
}

// ---------------------------------------------------------------------------
// Helper: convert our FtdiInterface to ftdi-nusb's Interface
// ---------------------------------------------------------------------------

fn map_interface(iface: FtdiInterface) -> ftdi_nusb::Interface {
    match iface {
        FtdiInterface::A => ftdi_nusb::Interface::A,
        FtdiInterface::B => ftdi_nusb::Interface::B,
        FtdiInterface::C => ftdi_nusb::Interface::C,
        FtdiInterface::D => ftdi_nusb::Interface::D,
    }
}

// ---------------------------------------------------------------------------
// Native-only methods (device enumeration, sync open, Drop)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "ftdi", not(target_arch = "wasm32")))]
impl Ftdi {
    /// Open an FTDI device with the given configuration
    pub async fn open(config: &FtdiConfig) -> Result<Self> {
        log::info!(
            "Opening FTDI {} channel {} (ftdi-nusb backend)",
            config.device_type.name(),
            config.interface.letter()
        );

        let interface = map_interface(config.interface);
        let vid = config.device_type.vendor_id();
        let pid = config.device_type.product_id();

        let mut filter = ftdi_nusb::DeviceFilter::new(vid, pid);
        if let Some(serial) = &config.serial {
            filter = filter.serial(serial);
        }
        if let Some(description) = &config.description {
            filter = filter.description(description);
        }

        log::debug!(
            "Looking for FTDI device VID={:04X} PID={:04X} serial={:?} description={:?}",
            vid,
            pid,
            config.serial,
            config.description
        );

        let mut device = FtdiDevice::open_with_filter(&filter, interface)
            .await
            .map_err(|e| FtdiError::OpenFailed(format!("{}", e)))?;

        log::debug!("Opened FTDI device VID={:04X} PID={:04X}", vid, pid);

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

        let ftdi = Self::configure(device, config).await?;

        log::info!(
            "FTDI configured for SPI at {:.2} MHz (ftdi-nusb backend)",
            config.spi_clock_mhz()
        );

        Ok(ftdi)
    }

    /// Open the first available FTDI device
    pub async fn open_first() -> Result<Self> {
        Self::open(&FtdiConfig::default()).await
    }

    /// Open a specific device type
    pub async fn open_device(device_type: FtdiDeviceType) -> Result<Self> {
        Self::open(&FtdiConfig::for_device(device_type)).await
    }

    /// List available FTDI devices
    pub async fn list_devices() -> Result<Vec<FtdiDeviceInfo>> {
        let devices = nusb::list_devices()
            .await
            .map_err(|e| FtdiError::UsbError(e.to_string()))?
            .filter_map(|dev| {
                let vid = dev.vendor_id();
                let pid = dev.product_id();

                get_device_info(vid, pid).map(|info| FtdiDeviceInfo {
                    bus_id: dev.bus_id().to_string(),
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

// Native-only best-effort cleanup; WASM uses explicit shutdown instead.
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Ftdi {
    fn drop(&mut self) {
        // Release I/O pins on close
        if let Err(e) = futures_lite::future::block_on(self.release_pins()) {
            log::warn!("Failed to release pins on close: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, async open, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl Ftdi {
    /// Request an FTDI device via the WebUSB permission prompt
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    /// It shows the browser's device picker filtered to all supported FTDI devices.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_device() -> Result<nusb::Device> {
        // Delegate to ftdi-nusb's WebUSB device picker
        ftdi_nusb::FtdiDevice::request_device()
            .await
            .map_err(|e| FtdiError::OpenFailed(format!("WebUSB request failed: {}", e)))
    }

    /// Open an FTDI device from a WebUSB-selected `nusb::Device` with the given configuration
    pub async fn open(device: nusb::Device, config: &FtdiConfig) -> Result<Self> {
        log::info!(
            "Opening FTDI {} channel {} (ftdi-nusb WebUSB)",
            config.device_type.name(),
            config.interface.letter()
        );

        let interface = map_interface(config.interface);

        let mut device = FtdiDevice::open_wasm(device, interface)
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

        let ftdi = Self::configure(device, config).await?;

        log::info!(
            "FTDI configured for SPI at {:.2} MHz (ftdi-nusb WebUSB)",
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
// Shared methods (async on every target)
// ---------------------------------------------------------------------------

impl Ftdi {
    async fn configure(mut device: FtdiDevice, config: &FtdiConfig) -> Result<Self> {
        let clock_hz = if config.device_type.is_high_speed() {
            60_000_000 / u32::from(config.divisor)
        } else {
            12_000_000 / u32::from(config.divisor)
        };
        log::debug!(
            "Setting clock divisor to {} (SPI clock: {:.2} MHz)",
            config.divisor,
            config.spi_clock_mhz()
        );

        let mut mpsse = MpsseContext::init(&mut device, clock_hz)
            .await
            .map_err(|e| FtdiError::ConfigFailed(format!("MPSSE init failed: {e}")))?;
        let spi = {
            let mut session = mpsse.session(&mut device)?;
            session.set_clock_divisor(config.divisor).await?;
            SpiDevice::with_config(
                &mut session,
                SpiConfig::new(SpiMode::Mode0)
                    .with_cs_mask(config.cs_bits, true)
                    .with_low_pins(config.aux_bits, config.pindir)
                    .with_high_pins(config.aux_bits_high, config.pindir_high),
            )
            .await?
        };

        Ok(Self { device, mpsse, spi })
    }

    /// Perform an SPI transfer through ftdi-nusb's MPSSE SPI implementation.
    async fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        if write_data.len() > 65536 || read_len > 65536 {
            return Err(FtdiError::TransferFailed(
                "Transfer length exceeds 64KB limit".to_string(),
            ));
        }

        let Self { device, mpsse, spi } = self;
        let mut session = mpsse.session(device)?;
        spi.write_read(&mut session, write_data, read_len)
            .await
            .map_err(Into::into)
    }

    /// Release I/O pins (set all as inputs).
    async fn release_pins(&mut self) -> Result<()> {
        let Self { device, mpsse, .. } = self;
        mpsse.session(device)?.release_pins().await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SpiMaster trait implementation
// ---------------------------------------------------------------------------

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
            #[cfg(not(target_arch = "wasm32"))]
            {
                std::thread::sleep(Duration::from_micros(us as u64));
            }

            #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
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
#[cfg(all(feature = "ftdi", not(target_arch = "wasm32")))]
#[derive(Debug, Clone)]
pub struct FtdiDeviceInfo {
    /// USB bus identifier (platform-defined; integer string on Linux)
    pub bus_id: String,
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

#[cfg(all(feature = "ftdi", not(target_arch = "wasm32")))]
impl std::fmt::Display for FtdiDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} at bus {} address {} ({:04X}:{:04X})",
            self.vendor_name,
            self.device_name,
            self.bus_id,
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
/// Format: `type=<type>,port=<A|B|C|D>,divisor=<N>,serial=<serial>,gpiol0=<H|L|C>`
#[cfg(all(feature = "ftdi", not(target_arch = "wasm32")))]
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
