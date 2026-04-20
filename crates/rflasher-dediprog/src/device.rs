//! Dediprog device implementation
//!
//! This module provides the main `Dediprog` struct that implements USB
//! communication with Dediprog SF100/SF200/SF600/SF700 programmers.
//!
//! Uses `maybe_async` to support both sync and async modes from a single
//! codebase:
//! - With `is_sync` feature (native CLI): all async is stripped, blocking USB
//! - Without `is_sync` (WASM): full async with WebUSB

use std::time::Duration;

use maybe_async::maybe_async;
use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::Endpoint;
#[cfg(feature = "std")]
use nusb::MaybeFuture;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use rflasher_core::protocol::SpiReadOp;
use rflasher_core::spi::{check_io_mode_supported, opcodes, IoMode as CoreIoMode, SpiCommand};

use crate::error::{DediprogError, Result};
use crate::protocol::*;

// ---------------------------------------------------------------------------
// Platform-specific endpoint wait macros
// ---------------------------------------------------------------------------
// These macros provide a uniform interface over nusb's blocking (native)
// and async (WASM) completion APIs.

/// Wait for the next completion on an endpoint, with timeout.
/// In sync mode: blocks with the given timeout.
/// In async mode: awaits indefinitely (timeout is ignored -- nusb's async
/// API does not support timeouts natively).
/// Returns `Option<Completion>`.
macro_rules! ep_wait {
    ($ep:expr, $timeout:expr) => {{
        #[cfg(feature = "is_sync")]
        {
            $ep.wait_next_complete($timeout)
        }
        #[cfg(not(feature = "is_sync"))]
        {
            Some($ep.next_complete().await)
        }
    }};
}

/// Resolve an nusb `MaybeFuture` to its output.
/// In sync mode: calls `.wait()` (blocking).
/// In async mode: `.await`s the future.
macro_rules! nusb_await {
    ($expr:expr) => {{
        #[cfg(feature = "is_sync")]
        {
            $expr.wait()
        }
        #[cfg(not(feature = "is_sync"))]
        {
            $expr.await
        }
    }};
}

/// Platform-aware sleep/delay.
/// In sync mode: std::thread::sleep.
/// In async mode (WASM): setTimeout-based delay.
macro_rules! platform_sleep {
    ($dur:expr) => {{
        #[cfg(feature = "is_sync")]
        {
            std::thread::sleep($dur);
        }
        #[cfg(all(feature = "wasm", not(feature = "is_sync")))]
        {
            // Browser setTimeout resolution is 1 ms minimum.  For sub-ms
            // durations we still yield via setTimeout(0) so the event loop
            // (and UI) can run -- without this, tight polling loops
            // (e.g. WIP status in slow_write) would busy-spin.
            let ms = $dur.as_millis() as i32;
            let promise = js_sys::Promise::new(&mut |resolve, _| {
                let window = web_sys::window().unwrap();
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
                    .unwrap();
            });
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        }
    }};
}

/// User-facing I/O-mode override policy for the programmer.
///
/// - `Auto`: advertise full multi-IO capability to the flash layer and let it
///   pick the best op from chip features (the default; matches `iomode=auto`).
/// - `Force(m)`: cap at the given mode regardless of chip capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoModePolicy {
    /// Let the flash layer decide based on chip + programmer capabilities.
    Auto,
    /// Force a specific upper bound.
    Force(DpIoMode),
}

impl Default for IoModePolicy {
    fn default() -> Self {
        IoModePolicy::Auto
    }
}

/// Configuration options for opening a Dediprog device
#[derive(Debug, Clone)]
pub struct DediprogConfig {
    /// Device index (when multiple devices are connected)
    pub device_index: usize,
    /// Device ID to search for (e.g., "SF123456")
    pub device_id: Option<String>,
    /// Target flash (1 or 2 for dual-chip programmers)
    pub target: Target,
    /// SPI speed index (0=24MHz, 1=12MHz, etc.)
    pub spi_speed_index: usize,
    /// Voltage in millivolts (0, 1800, 2500, 3500)
    pub voltage_mv: u16,
    /// I/O mode policy (auto or forced cap)
    pub io_mode_policy: IoModePolicy,
}

impl Default for DediprogConfig {
    fn default() -> Self {
        Self {
            device_index: 0,
            device_id: None,
            target: Target::ApplicationFlash1,
            spi_speed_index: DEFAULT_SPI_SPEED_INDEX,
            voltage_mv: DEFAULT_VOLTAGE_MV,
            io_mode_policy: IoModePolicy::Auto,
        }
    }
}

/// Parse options from key=value pairs
pub fn parse_options(options: &[(&str, &str)]) -> Result<DediprogConfig> {
    let mut config = DediprogConfig::default();

    for (key, value) in options {
        match *key {
            "device" | "index" => {
                config.device_index = value
                    .parse()
                    .map_err(|_| DediprogError::InvalidParameter(format!("device: {}", value)))?;
            }
            "id" => {
                config.device_id = Some(value.to_string());
            }
            "target" => {
                let t: u8 = value
                    .parse()
                    .map_err(|_| DediprogError::InvalidParameter(format!("target: {}", value)))?;
                config.target = Target::from_value(t)
                    .ok_or_else(|| DediprogError::InvalidParameter(format!("target: {}", value)))?;
            }
            "spispeed" => {
                config.spi_speed_index = parse_spi_speed(value).ok_or_else(|| {
                    DediprogError::InvalidParameter(format!("spispeed: {}", value))
                })?;
            }
            "voltage" => {
                config.voltage_mv = parse_voltage(value).ok_or_else(|| {
                    DediprogError::InvalidParameter(format!("voltage: {}", value))
                })?;
            }
            "iomode" => {
                config.io_mode_policy = match value.to_lowercase().as_str() {
                    "auto" => IoModePolicy::Auto,
                    "single" | "1" => IoModePolicy::Force(DpIoMode::Single),
                    "dual" | "2" => IoModePolicy::Force(DpIoMode::DualIo),
                    "quad" | "4" => IoModePolicy::Force(DpIoMode::QuadIo),
                    _ => {
                        return Err(DediprogError::InvalidParameter(format!(
                            "iomode: {}",
                            value
                        )));
                    }
                };
            }
            _ => {
                return Err(DediprogError::InvalidParameter(format!(
                    "unknown option: {}",
                    key
                )));
            }
        }
    }

    Ok(config)
}

/// Dediprog USB programmer
///
/// Supports SF100, SF200, SF600, SF600PG2, and SF700 programmers.
pub struct Dediprog {
    /// USB interface handle (used for control transfers in native mode;
    /// kept alive to maintain device claim in WASM mode)
    #[allow(dead_code)] // In WASM, only accessed via iface() helper
    interface: nusb::Interface,
    /// Bulk IN endpoint
    in_endpoint: u8,
    /// Bulk OUT endpoint
    out_endpoint: u8,
    /// Device type
    device_type: DeviceType,
    /// Firmware version (encoded as major<<16 | minor<<8 | patch)
    firmware_version: u32,
    /// Device string (e.g., "SF600 V:7.2.0")
    device_string: String,
    /// Protocol version
    protocol: Protocol,
    /// Current I/O mode
    io_mode: DpIoMode,
    /// Configured maximum I/O mode
    max_io_mode: DpIoMode,
    /// User-selected I/O-mode policy (auto or forced)
    io_mode_policy: IoModePolicy,
    /// Flash size in bytes (set after probing, needed for OpaqueMaster)
    flash_size: Option<u32>,
    /// Chip-selected read op (set via `set_read_op`); None until prepared.
    selected_read_op: Option<SpiReadOp>,
}

impl Dediprog {
    #[inline]
    fn iface(&self) -> &nusb::Interface {
        &self.interface
    }
}

// ---------------------------------------------------------------------------
// Native-only methods (device enumeration, Drop)
// ---------------------------------------------------------------------------

#[cfg(feature = "std")]
impl Dediprog {
    /// Open the first available Dediprog device
    pub fn open() -> Result<Self> {
        Self::open_with_config(DediprogConfig::default())
    }

    /// Open a Dediprog device with the specified configuration
    pub fn open_with_config(config: DediprogConfig) -> Result<Self> {
        // Find matching devices
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?
            .filter(|d| {
                d.vendor_id() == DEDIPROG_USB_VENDOR && d.product_id() == DEDIPROG_USB_PRODUCT
            })
            .collect();

        if devices.is_empty() {
            return Err(DediprogError::DeviceNotFound);
        }

        // If searching by ID, try each device
        if let Some(ref target_id) = config.device_id {
            for device_info in &devices {
                match Self::try_open_device(device_info, &config) {
                    Ok(mut dediprog) => {
                        // Read device ID and check
                        if let Ok(id) = dediprog.read_device_id() {
                            let id_str = format!("SF{:06}", id);
                            if id_str.contains(target_id) || target_id.contains(&id_str) {
                                log::info!("Found Dediprog with ID {}", id_str);
                                return Ok(dediprog);
                            }
                        }
                        // Close and try next
                        drop(dediprog);
                    }
                    Err(_) => continue,
                }
            }
            return Err(DediprogError::DeviceNotFound);
        }

        // Open by index
        let device_info = devices
            .get(config.device_index)
            .ok_or(DediprogError::DeviceNotFound)?;

        Self::try_open_device(device_info, &config)
    }

    /// Try to open a specific USB device (native/blocking)
    fn try_open_device(device_info: &nusb::DeviceInfo, config: &DediprogConfig) -> Result<Self> {
        log::info!(
            "Opening Dediprog at bus {} address {}",
            device_info.busnum(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .wait()
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?;

        // Claim interface 0
        let interface = device
            .claim_interface(0)
            .wait()
            .map_err(|e| DediprogError::ClaimFailed(e.to_string()))?;

        let mut dediprog = Self {
            interface,
            in_endpoint: BULK_IN_EP,
            out_endpoint: BULK_OUT_EP_SF100, // Will be updated based on device type
            device_type: DeviceType::Unknown,
            firmware_version: 0,
            device_string: String::new(),
            protocol: Protocol::Unknown,
            io_mode: DpIoMode::Single,
            max_io_mode: DpIoMode::Single, // set by init_device() below
            io_mode_policy: config.io_mode_policy,
            flash_size: None,
            selected_read_op: None,
        };

        dediprog.init_device(config)?;
        Ok(dediprog)
    }

    /// List all connected Dediprog devices
    pub fn list_devices() -> Result<Vec<DediprogDeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?
            .filter(|d| {
                d.vendor_id() == DEDIPROG_USB_VENDOR && d.product_id() == DEDIPROG_USB_PRODUCT
            })
            .map(|d| DediprogDeviceInfo {
                bus: d.busnum(),
                address: d.device_address(),
            })
            .collect();

        Ok(devices)
    }
}

/// Information about a connected Dediprog device
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub struct DediprogDeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
}

#[cfg(feature = "std")]
impl std::fmt::Display for DediprogDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dediprog at bus {} address {}", self.bus, self.address)
    }
}

// Drop implementation only for sync mode (async requires explicit shutdown)
#[cfg(feature = "is_sync")]
impl Drop for Dediprog {
    fn drop(&mut self) {
        // Reset I/O mode
        let _ = self.set_io_mode(DpIoMode::Single);
        // Turn off voltage
        let _ = self.set_voltage(0);
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, async open, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", not(feature = "is_sync")))]
impl Dediprog {
    /// Request a Dediprog device via the WebUSB permission prompt
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    /// It shows the browser's device picker filtered to Dediprog devices.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        use web_sys::{UsbDevice, UsbDeviceFilter, UsbDeviceRequestOptions};

        let usb = web_sys::window()
            .ok_or(DediprogError::DeviceNotFound)?
            .navigator()
            .usb();

        // Create filter for Dediprog devices (VID:0483 PID:DADA)
        let filter = UsbDeviceFilter::new();
        filter.set_vendor_id(DEDIPROG_USB_VENDOR);
        filter.set_product_id(DEDIPROG_USB_PRODUCT);

        let filters = js_sys::Array::new();
        filters.push(&filter);

        let options = UsbDeviceRequestOptions::new(&filters);

        log::info!("Requesting Dediprog device via WebUSB picker...");

        let device_promise = usb.request_device(&options);
        let device_js = JsFuture::from(device_promise)
            .await
            .map_err(|e| DediprogError::OpenFailed(format!("WebUSB request failed: {:?}", e)))?;

        let device: UsbDevice = device_js
            .dyn_into()
            .map_err(|_| DediprogError::OpenFailed("Failed to get USB device".to_string()))?;

        log::info!(
            "Dediprog device selected: VID={:04X} PID={:04X}",
            device.vendor_id(),
            device.product_id()
        );

        let device_info = nusb::device_info_from_webusb(device)
            .await
            .map_err(|e| DediprogError::OpenFailed(format!("Failed to get device info: {}", e)))?;

        Ok(device_info)
    }

    /// Open a Dediprog device from a DeviceInfo (async, for WASM)
    pub async fn open(device_info: nusb::DeviceInfo, config: DediprogConfig) -> Result<Self> {
        log::info!(
            "Opening Dediprog device VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        let device = device_info
            .open()
            .await
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?;

        let interface = device
            .claim_interface(0)
            .await
            .map_err(|e| DediprogError::ClaimFailed(e.to_string()))?;

        let mut dediprog = Self {
            interface,
            in_endpoint: BULK_IN_EP,
            out_endpoint: BULK_OUT_EP_SF100,
            device_type: DeviceType::Unknown,
            firmware_version: 0,
            device_string: String::new(),
            protocol: Protocol::Unknown,
            io_mode: DpIoMode::Single,
            max_io_mode: DpIoMode::Single, // set by init_device() below
            io_mode_policy: config.io_mode_policy,
            flash_size: None,
            selected_read_op: None,
        };

        dediprog.init_device(&config).await?;
        Ok(dediprog)
    }

    /// Shutdown: turn off voltage and reset I/O mode (WASM equivalent of Drop)
    pub async fn shutdown(&mut self) {
        let _ = self.set_io_mode(DpIoMode::Single).await;
        let _ = self.set_voltage(0).await;
    }
}

// ---------------------------------------------------------------------------
// Shared methods (sync or async via maybe_async)
// ---------------------------------------------------------------------------

// When all features are enabled simultaneously (e.g. --all-features in CI),
// the mutually-exclusive open() methods are both excluded, making these shared
// helpers appear unused. Allow dead_code for that configuration.
#[cfg_attr(all(feature = "wasm", feature = "is_sync"), allow(dead_code))]
impl Dediprog {
    /// Initialize device after USB connection is established.
    /// Shared between native and WASM paths.
    #[maybe_async]
    async fn init_device(&mut self, config: &DediprogConfig) -> Result<()> {
        // Try to read device string (may need set_voltage first for old devices)
        if self.read_device_string().await.is_err() {
            // Try set_voltage for old firmware and retry
            self.set_voltage_old().await?;
            self.read_device_string().await?;
        }

        // Update endpoints based on device type
        if self.device_type.is_sf600_class() {
            self.out_endpoint = BULK_OUT_EP_SF600;
        }

        // Determine protocol version
        self.protocol = Protocol::from_device_firmware(self.device_type, self.firmware_version);

        if self.protocol == Protocol::Unknown {
            return Err(DediprogError::FirmwareError(
                "Unable to determine protocol version".to_string(),
            ));
        }

        log::info!(
            "Dediprog {}: firmware {:X}.{:X}.{:X}, protocol {:?}",
            self.device_type,
            (self.firmware_version >> 16) & 0xFF,
            (self.firmware_version >> 8) & 0xFF,
            self.firmware_version & 0xFF,
            self.protocol
        );

        // Initialize the device
        self.set_leds(Led::All).await?;

        // Set target, speed, and voltage
        self.set_target(config.target).await?;
        self.set_spi_speed(config.spi_speed_index).await?;
        self.set_voltage(config.voltage_mv).await?;

        // Leave standalone mode if SF600
        if self.device_type == DeviceType::SF600 {
            self.leave_standalone_mode().await?;
        }

        // Determine multi-I/O support
        if self.device_type.is_sf600_class() && self.protocol >= Protocol::V2 {
            self.max_io_mode = match self.io_mode_policy {
                IoModePolicy::Auto => DpIoMode::QuadIo,
                IoModePolicy::Force(m) => m,
            };
        } else {
            self.max_io_mode = DpIoMode::Single;
        }

        self.set_leds(Led::None).await?;

        Ok(())
    }

    /// Read the device string and parse device type/firmware
    #[maybe_async]
    async fn read_device_string(&mut self) -> Result<()> {
        let mut buf = [0u8; 33];
        let len = self
            .control_read(Command::ReadProgInfo, 0, 0, &mut buf)
            .await?;

        if len < 16 {
            return Err(DediprogError::InvalidResponse(
                "Device string too short".to_string(),
            ));
        }

        self.device_string = String::from_utf8_lossy(&buf[..len])
            .trim_end_matches('\0')
            .to_string();

        log::debug!("Device string: {}", self.device_string);

        // Parse device type
        self.device_type = DeviceType::from_device_string(&self.device_string);
        if self.device_type == DeviceType::Unknown {
            return Err(DediprogError::UnknownDevice(self.device_string.clone()));
        }

        // Parse firmware version (format: "SFXXX V:X.X.X")
        if let Some(version_str) = self.device_string.split("V:").nth(1) {
            let parts: Vec<&str> = version_str.split('.').collect();
            if parts.len() >= 3 {
                let major: u32 = parts[0].parse().unwrap_or(0);
                let minor: u32 = parts[1].parse().unwrap_or(0);
                let patch: u32 = parts[2]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
                self.firmware_version = firmware_version(major, minor, patch);
            }
        }

        // Verify firmware version is in expected range
        let major = (self.firmware_version >> 16) & 0xFF;
        match self.device_type {
            DeviceType::SF600PG2 if major > 1 => {
                return Err(DediprogError::FirmwareError(format!(
                    "Unexpected firmware version for SF600PG2: {}",
                    self.device_string
                )));
            }
            DeviceType::SF700 if major != 4 => {
                return Err(DediprogError::FirmwareError(format!(
                    "Unexpected firmware version for SF700: {}",
                    self.device_string
                )));
            }
            DeviceType::SF100 | DeviceType::SF200 | DeviceType::SF600
                if !(2..=7).contains(&major) =>
            {
                return Err(DediprogError::FirmwareError(format!(
                    "Unexpected firmware version: {}",
                    self.device_string
                )));
            }
            _ => {}
        }

        Ok(())
    }

    /// Read the device ID (serial number from sticker)
    #[maybe_async]
    #[allow(dead_code)] // Only called from native open_with_config
    async fn read_device_id(&mut self) -> Result<u32> {
        if self.device_type >= DeviceType::SF600PG2 {
            // Newer protocol for SF600PG2/SF700
            let out = [0x00, 0x00, 0x00, 0x02, 0x00, 0x00];
            self.control_write_raw(0x71, 0, 0, &out).await?;

            let mut buf = [0u8; 512];
            let len = self.bulk_read(&mut buf).await?;
            if len >= 3 {
                return Ok((buf[2] as u32) << 16 | (buf[1] as u32) << 8 | (buf[0] as u32));
            }
        } else if self.device_type.is_sf600_class() {
            // SF600 uses CMD_READ_EEPROM
            let mut buf = [0u8; 16];
            let len = self
                .control_read(Command::ReadEeprom, 0, 0, &mut buf)
                .await?;
            if len >= 3 {
                return Ok((buf[0] as u32) << 16 | (buf[1] as u32) << 8 | (buf[2] as u32));
            }
        } else {
            // SF100/SF200 use a different request
            let mut buf = [0u8; 3];
            let len = self
                .control_read_raw(REQTYPE_OTHER_IN, 0x07, 0, 0xEF00, &mut buf)
                .await?;
            if len >= 3 {
                return Ok((buf[0] as u32) << 16 | (buf[1] as u32) << 8 | (buf[2] as u32));
            }
        }

        Err(DediprogError::InvalidResponse(
            "Failed to read device ID".to_string(),
        ))
    }

    /// Set voltage for old firmware (< 6.0.0)
    #[maybe_async]
    async fn set_voltage_old(&mut self) -> Result<()> {
        let mut buf = [0u8; 1];
        let ret = self
            .control_read_raw(REQTYPE_OTHER_IN, Command::SetVoltage as u8, 0, 0, &mut buf)
            .await?;
        if ret != 1 || buf[0] != 0x6f {
            return Err(DediprogError::InvalidResponse(
                "Unexpected response to set_voltage".to_string(),
            ));
        }
        Ok(())
    }

    /// Set the LED state
    #[maybe_async]
    async fn set_leds(&mut self, led: Led) -> Result<()> {
        if self.protocol >= Protocol::V2 {
            // New protocol: value contains LED state
            let leds = ((led as u8) ^ 7) as u16;
            self.control_write(Command::SetIoLed, leds << 8, 0, &[])
                .await?;
        } else {
            // Old protocol: index contains LED state
            let leds = if self.firmware_version < firmware_version(5, 0, 0) {
                // Very old firmware has different LED mapping
                let l = led as u8;
                ((l & 4) >> 2) | ((l & 1) << 2)
            } else {
                led as u8
            };
            let target_leds = leds ^ 7;
            self.control_write(Command::SetIoLed, 0x9, target_leds as u16, &[])
                .await?;
        }
        Ok(())
    }

    /// Set the target flash
    #[maybe_async]
    async fn set_target(&mut self, target: Target) -> Result<()> {
        self.control_write(Command::SetTarget, target as u16, 0, &[])
            .await?;
        Ok(())
    }

    /// Set the SPI clock speed
    #[maybe_async]
    async fn set_spi_speed(&mut self, speed_index: usize) -> Result<()> {
        if self.device_type < DeviceType::SF600PG2
            && self.firmware_version < firmware_version(5, 0, 0)
        {
            log::warn!("Skipping SPI speed setting for old firmware");
            return Ok(());
        }

        let speed = SPI_SPEEDS.get(speed_index).ok_or_else(|| {
            DediprogError::InvalidParameter("Invalid SPI speed index".to_string())
        })?;

        log::debug!("Setting SPI speed to {}", speed.name);
        self.control_write(Command::SetSpiClk, speed.value as u16, 0, &[])
            .await?;
        Ok(())
    }

    /// Set the SPI voltage
    #[maybe_async]
    async fn set_voltage(&mut self, millivolt: u16) -> Result<()> {
        let selector = voltage_selector(millivolt)
            .ok_or_else(|| DediprogError::InvalidParameter(format!("voltage: {}", millivolt)))?;

        log::debug!(
            "Setting SPI voltage to {}.{:03}V",
            millivolt / 1000,
            millivolt % 1000
        );

        if selector == 0 {
            // Delay before turning off voltage
            platform_sleep!(Duration::from_millis(200));
        }

        self.control_write(Command::SetVcc, selector, 0, &[])
            .await?;

        if selector != 0 {
            // Delay after turning on voltage
            platform_sleep!(Duration::from_millis(200));
        }

        Ok(())
    }

    /// Leave standalone mode (SF600 only)
    #[maybe_async]
    async fn leave_standalone_mode(&mut self) -> Result<()> {
        if self.device_type != DeviceType::SF600 {
            return Ok(());
        }

        log::debug!("Leaving standalone mode");
        self.control_write(Command::SetStandalone, StandaloneMode::Leave as u16, 0, &[])
            .await?;
        Ok(())
    }

    /// Set the I/O mode for multi-I/O operations
    #[maybe_async]
    async fn set_io_mode(&mut self, mode: DpIoMode) -> Result<()> {
        if !self.device_type.is_sf600_class() {
            return Ok(());
        }

        if self.io_mode == mode {
            return Ok(());
        }

        log::trace!("Setting I/O mode to {:?}", mode);
        self.control_write(Command::IoMode, mode as u16, 0, &[])
            .await?;
        self.io_mode = mode;
        Ok(())
    }

    /// USB control read
    #[maybe_async]
    async fn control_read(
        &mut self,
        cmd: Command,
        value: u16,
        index: u16,
        buf: &mut [u8],
    ) -> Result<usize> {
        self.control_read_raw(REQTYPE_EP_IN, cmd as u8, value, index, buf)
            .await
    }

    /// USB control read (raw)
    #[maybe_async]
    async fn control_read_raw(
        &mut self,
        #[allow(unused_variables)] request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
    ) -> Result<usize> {
        // On WASM/WebUSB, Chrome validates the `index` field as an endpoint
        // address when recipient is Endpoint, rejecting Dediprog's
        // protocol-specific index values with IndexSizeError.  The firmware
        // dispatches on bRequest only, so Device works fine.
        #[cfg(feature = "is_sync")]
        let recipient = if request_type & 0x03 == 0x02 {
            nusb::transfer::Recipient::Endpoint
        } else {
            nusb::transfer::Recipient::Other
        };
        #[cfg(not(feature = "is_sync"))]
        let recipient = nusb::transfer::Recipient::Device;

        let data = nusb_await!(self.iface().control_in(
            nusb::transfer::ControlIn {
                control_type: nusb::transfer::ControlType::Vendor,
                recipient,
                request,
                value,
                index,
                length: buf.len() as u16,
            },
            Duration::from_secs(5),
        ))
        .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    /// USB control write
    #[maybe_async]
    async fn control_write(
        &mut self,
        cmd: Command,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        self.control_write_raw(cmd as u8, value, index, data).await
    }

    /// USB control write (raw)
    #[maybe_async]
    async fn control_write_raw(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        // See control_read_raw for rationale on the recipient override.
        #[cfg(feature = "is_sync")]
        let recipient = nusb::transfer::Recipient::Endpoint;
        #[cfg(not(feature = "is_sync"))]
        let recipient = nusb::transfer::Recipient::Device;

        nusb_await!(self.iface().control_out(
            nusb::transfer::ControlOut {
                control_type: nusb::transfer::ControlType::Vendor,
                recipient,
                request,
                value,
                index,
                data,
            },
            Duration::from_secs(5),
        ))
        .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    /// Bulk read
    #[maybe_async]
    #[allow(dead_code)] // Only called from read_device_id (native path)
    async fn bulk_read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut in_ep: Endpoint<Bulk, In> = self
            .iface()
            .endpoint(self.in_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let max_packet_size = in_ep.max_packet_size();
        let request_len = buf.len().div_ceil(max_packet_size) * max_packet_size;
        let mut in_buf = Buffer::new(request_len);
        in_buf.set_requested_len(request_len);

        in_ep.submit(in_buf);
        let completion = ep_wait!(in_ep, Duration::from_secs(5)).ok_or(DediprogError::Timeout)?;
        completion
            .status
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let data = &completion.buffer[..];
        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    /// Bulk write
    #[allow(dead_code)]
    #[maybe_async]
    async fn bulk_write(&mut self, data: &[u8]) -> Result<()> {
        let mut out_ep: Endpoint<Bulk, Out> = self
            .iface()
            .endpoint(self.out_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let mut out_buf = Buffer::new(data.len());
        out_buf.extend_from_slice(data);

        out_ep.submit(out_buf);
        let completion = ep_wait!(out_ep, Duration::from_secs(5)).ok_or(DediprogError::Timeout)?;
        completion
            .status
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    /// Send a transceive command (generic SPI command)
    #[maybe_async]
    async fn spi_transceive(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        // Set to single I/O mode for generic commands
        self.set_io_mode(DpIoMode::Single).await?;

        // Build command
        let (value, index) = if self.protocol >= Protocol::V2 {
            // New protocol: value indicates if we need a read
            (if read_len > 0 { 0x1 } else { 0x0 }, 0)
        } else {
            // Old protocol: index indicates if we need a read
            (0, if read_len > 0 { 0x1 } else { 0x0 })
        };

        // Send command
        self.control_write(Command::Transceive, value, index, write_data)
            .await?;

        if read_len == 0 {
            return Ok(Vec::new());
        }

        // Read response
        let mut buf = vec![0u8; read_len];
        let mut total_read = 0;

        while total_read < read_len {
            let to_read = (read_len - total_read).min(64);

            // See control_read_raw for rationale on the recipient override.
            #[cfg(feature = "is_sync")]
            let recipient = nusb::transfer::Recipient::Endpoint;
            #[cfg(not(feature = "is_sync"))]
            let recipient = nusb::transfer::Recipient::Device;

            let data = nusb_await!(self.iface().control_in(
                nusb::transfer::ControlIn {
                    control_type: nusb::transfer::ControlType::Vendor,
                    recipient,
                    request: Command::Transceive as u8,
                    value: 0,
                    index: 0,
                    length: to_read as u16,
                },
                Duration::from_secs(5),
            ))
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

            let len = data.len().min(to_read);
            buf[total_read..total_read + len].copy_from_slice(&data[..len]);
            total_read += len;

            if data.len() < to_read {
                break;
            }
        }

        Ok(buf)
    }

    /// Get the device type
    pub fn device_type(&self) -> DeviceType {
        self.device_type
    }

    /// Get the device string
    pub fn device_string(&self) -> &str {
        &self.device_string
    }

    /// Get the firmware version (encoded)
    pub fn firmware_version(&self) -> u32 {
        self.firmware_version
    }

    /// Get the protocol version
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Set the flash size (call after probing to enable OpaqueMaster)
    pub fn set_flash_size(&mut self, size: u32) {
        self.flash_size = Some(size);
    }

    // =========================================================================
    // Bulk Read/Write (CMD_READ/CMD_WRITE with USB bulk transfers)
    // =========================================================================

    /// Prepare a read/write command packet for the given protocol version.
    ///
    /// Returns the command packet size. The packet is written into `cmd_buf`.
    /// `value` and `idx` are the USB control transfer wValue/wIndex fields.
    #[allow(clippy::too_many_arguments)]
    fn prepare_rw_cmd(
        &self,
        cmd_buf: &mut [u8; MAX_CMD_SIZE],
        value: &mut u16,
        idx: &mut u16,
        is_read: bool,
        mode: u8,
        start: u32,
        count: u16,
    ) -> Result<usize> {
        // Common header (all protocol versions)
        cmd_buf[0] = (count & 0xFF) as u8;
        cmd_buf[1] = ((count >> 8) & 0xFF) as u8;
        cmd_buf[2] = 0; // RFU
        cmd_buf[3] = mode;
        cmd_buf[4] = 0; // Opcode (overridden below for V2/V3)

        match self.protocol {
            Protocol::V1 => {
                // V1: address in wValue/wIndex, 5-byte command packet
                if start >> 24 != 0 {
                    return Err(DediprogError::Unsupported(
                        "4-byte address not supported on V1 protocol".to_string(),
                    ));
                }
                *value = (start & 0xFFFF) as u16;
                *idx = ((start >> 16) & 0xFF) as u16;
                Ok(5)
            }
            Protocol::V2 => {
                *value = 0;
                *idx = 0;

                if is_read {
                    // For V2 reads, translate the selected SpiReadOp into the
                    // right ReadMode + opcode. V2 can't express dummy cycles
                    // other than the fixed defaults so we pick the closest.
                    let op = self.selected_read_op.unwrap_or(SpiReadOp {
                        opcode: opcodes::FAST_READ,
                        io_mode: CoreIoMode::Single,
                        dummy_cycles: 8,
                        native_4ba: false,
                    });
                    if op.native_4ba {
                        cmd_buf[3] = ReadMode::FourByteAddrFast as u8;
                    } else {
                        cmd_buf[3] = ReadMode::Fast as u8;
                    }
                    cmd_buf[4] = op.opcode;
                } else {
                    // For V2 writes, use page program mode
                    cmd_buf[3] = WriteMode::PagePgm as u8;
                    cmd_buf[4] = 0;
                }

                cmd_buf[5] = 0; // RFU
                cmd_buf[6] = (start & 0xFF) as u8;
                cmd_buf[7] = ((start >> 8) & 0xFF) as u8;
                cmd_buf[8] = ((start >> 16) & 0xFF) as u8;
                cmd_buf[9] = ((start >> 24) & 0xFF) as u8;
                Ok(10)
            }
            Protocol::V3 => {
                *value = 0;
                *idx = 0;

                cmd_buf[5] = 0; // RFU
                cmd_buf[6] = (start & 0xFF) as u8;
                cmd_buf[7] = ((start >> 8) & 0xFF) as u8;
                cmd_buf[8] = ((start >> 16) & 0xFF) as u8;
                cmd_buf[9] = ((start >> 24) & 0xFF) as u8;

                if is_read {
                    // V3 supports ReadMode::Configurable which lets us pass
                    // opcode + address width + dummy half-cycles, which the
                    // firmware uses with the currently-configured IO mode.
                    let op = self.selected_read_op.unwrap_or(SpiReadOp {
                        opcode: opcodes::FAST_READ,
                        io_mode: CoreIoMode::Single,
                        dummy_cycles: 8,
                        native_4ba: false,
                    });
                    cmd_buf[3] = ReadMode::Configurable as u8;
                    cmd_buf[4] = op.opcode;
                    cmd_buf[10] = if op.native_4ba || start >> 24 != 0 {
                        4
                    } else {
                        3
                    };
                    // dediprog firmware handles the mode byte for opcodes
                    // that require one (0xBB/0xEB/0xBC/0xEC). Our
                    // SpiReadOp.dummy_cycles counts total SCLK cycles
                    // between end-of-address and start-of-data, which
                    // includes mode-byte time for 1-x-x / QPI. flashprog's
                    // spi_dummy_cycles subtracts the mode-byte time before
                    // passing to the firmware — do the same here.
                    let mode_byte_clocks = match op.io_mode {
                        CoreIoMode::DualIo => 4, // 8 bits / 2 wires
                        CoreIoMode::QuadIo | CoreIoMode::Qpi => 2, // 8 bits / 4 wires
                        _ => 0,
                    };
                    let dummy_after_mode =
                        op.dummy_cycles.saturating_sub(mode_byte_clocks);
                    // cmd_buf[11] encodes dummy time in units of 2 SCLK
                    // (flashprog divides spi_dummy_cycles by 2 when writing
                    // this byte). Round up so a chip expecting an odd
                    // number of dummy clocks still gets at least that many.
                    cmd_buf[11] = dummy_after_mode.div_ceil(2);
                    Ok(12)
                } else {
                    cmd_buf[3] = WriteMode::PagePgm as u8;
                    cmd_buf[4] = 0;
                    // Page size (256 bytes) as 32-bit LE
                    cmd_buf[10] = 0x00;
                    cmd_buf[11] = 0x01;
                    cmd_buf[12] = 0x00;
                    cmd_buf[13] = 0x00;
                    Ok(14)
                }
            }
            Protocol::Unknown => Err(DediprogError::Unsupported(
                "Unknown protocol version".to_string(),
            )),
        }
    }

    /// Bulk read from flash using CMD_READ + USB bulk IN transfers.
    ///
    /// Start and len MUST be 512-byte aligned. Uses a single large URB so the
    /// kernel handles all USB scheduling internally -- avoids per-packet
    /// userspace round-trips through nusb's epoll background thread.
    #[maybe_async]
    async fn bulk_read_flash(&mut self, start: u32, buf: &mut [u8]) -> Result<()> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }

        let count = (len / BULK_CHUNK_SIZE) as u16;

        // Pick the dediprog IO mode from the selected read op (if any).
        // Fall back to Single if nothing was set.
        let dp_mode = match self.selected_read_op {
            Some(op) => DpIoMode::from(op.io_mode),
            None => DpIoMode::Single,
        };
        self.set_io_mode(dp_mode).await?;

        // Build and send the CMD_READ command packet
        let mut cmd_buf = [0u8; MAX_CMD_SIZE];
        let mut value: u16 = 0;
        let mut idx: u16 = 0;
        let cmd_len = self.prepare_rw_cmd(
            &mut cmd_buf,
            &mut value,
            &mut idx,
            true,
            ReadMode::Std as u8,
            start,
            count,
        )?;

        self.control_write_raw(Command::Read as u8, value, idx, &cmd_buf[..cmd_len])
            .await?;

        // Submit a single large bulk IN transfer for the entire read.
        let mut in_ep: Endpoint<Bulk, In> = self
            .iface()
            .endpoint(self.in_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let xfer_buf = in_ep.allocate(len);
        in_ep.submit(xfer_buf);

        // Scale timeout with transfer size: 10 s base + ~30 us per byte
        // (accommodates the slowest SPI speed of 375 kHz ~ 47 KiB/s)
        let _timeout =
            Duration::from_secs(ASYNC_TIMEOUT_SECS) + Duration::from_micros(len as u64 * 30);

        let result = ep_wait!(in_ep, _timeout).ok_or(DediprogError::Timeout)?;
        result
            .status
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        if result.actual_len != len {
            return Err(DediprogError::TransferFailed(format!(
                "Short bulk read: expected {} bytes, got {}",
                len, result.actual_len
            )));
        }

        buf.copy_from_slice(&result.buffer[..len]);
        Ok(())
    }

    /// Bulk write to flash using CMD_WRITE + USB bulk OUT transfers.
    ///
    /// Start and len MUST be 256-byte aligned. Builds a single contiguous
    /// USB buffer with each 256-byte page padded to 512 bytes (0xFF fill),
    /// then submits it as one large URB. The firmware reads 512 bytes at a
    /// time and handles WREN, page program, and WIP polling internally.
    #[maybe_async]
    async fn bulk_write_flash(&mut self, start: u32, data: &[u8]) -> Result<()> {
        const PAGE_SIZE: usize = 256;
        let len = data.len();
        if len == 0 {
            return Ok(());
        }

        let count = (len / PAGE_SIZE) as u16;

        // Writes always use single I/O
        self.set_io_mode(DpIoMode::Single).await?;

        // Build and send the CMD_WRITE command packet
        let mut cmd_buf = [0u8; MAX_CMD_SIZE];
        let mut value: u16 = 0;
        let mut idx: u16 = 0;
        let cmd_len = self.prepare_rw_cmd(
            &mut cmd_buf,
            &mut value,
            &mut idx,
            false,
            WriteMode::PagePgm as u8,
            start,
            count,
        )?;

        self.control_write_raw(Command::Write as u8, value, idx, &cmd_buf[..cmd_len])
            .await?;

        // Build a single padded buffer: for each 256-byte page, write 256 data + 256 0xFF.
        // The firmware consumes 512 bytes per page and handles the SPI protocol internally.
        let mut out_ep: Endpoint<Bulk, Out> = self
            .iface()
            .endpoint(self.out_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let count = count as usize;
        let total_usb_len = count * BULK_CHUNK_SIZE;
        let mut out_buf = out_ep.allocate(total_usb_len);

        for i in 0..count {
            let src_start = i * PAGE_SIZE;
            out_buf.extend_from_slice(&data[src_start..src_start + PAGE_SIZE]);
            out_buf.extend_from_slice(&[0xFF; BULK_CHUNK_SIZE - PAGE_SIZE]);
        }

        out_ep.submit(out_buf);

        // Scale timeout with transfer size: 10 s base + 10 ms per page
        // (accommodates worst-case page-program time of typical NOR flash)
        let _timeout =
            Duration::from_secs(ASYNC_TIMEOUT_SECS) + Duration::from_millis(count as u64 * 10);

        let result = ep_wait!(out_ep, _timeout).ok_or(DediprogError::Timeout)?;
        result
            .status
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    /// Slow read via SPI transceive (for unaligned head/tail residuals).
    /// Reads up to 16 bytes per USB control transfer using standard READ (0x03).
    #[maybe_async]
    async fn slow_read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        let mut offset = 0usize;
        while offset < buf.len() {
            let chunk_len = (buf.len() - offset).min(16);
            let a = addr + offset as u32;
            let cmd = [opcodes::READ, (a >> 16) as u8, (a >> 8) as u8, a as u8];
            let result = self.spi_transceive(&cmd, chunk_len).await?;
            buf[offset..offset + chunk_len].copy_from_slice(&result[..chunk_len]);
            offset += chunk_len;
        }
        Ok(())
    }

    /// Slow write via SPI transceive (for unaligned head/tail residuals).
    /// Sends individual WREN + PP + RDSR poll sequences, max 11 bytes data per transfer.
    #[maybe_async]
    async fn slow_write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let max_write = 16 - 5; // 11 bytes per transceive (16 - 1 opcode - 3 addr - 1 margin)
        let mut offset = 0usize;

        while offset < data.len() {
            let a = addr + offset as u32;
            // Respect page boundaries (256 bytes)
            let page_offset = a as usize % 256;
            let to_page_end = 256 - page_offset;
            let remaining = data.len() - offset;
            let chunk_len = remaining.min(max_write).min(to_page_end);

            // WREN
            self.spi_transceive(&[opcodes::WREN], 0).await?;

            // Page Program: [PP, addr_hi, addr_mid, addr_lo, data...]
            let mut cmd = Vec::with_capacity(4 + chunk_len);
            cmd.push(opcodes::PP);
            cmd.push((a >> 16) as u8);
            cmd.push((a >> 8) as u8);
            cmd.push(a as u8);
            cmd.extend_from_slice(&data[offset..offset + chunk_len]);
            self.spi_transceive(&cmd, 0).await?;

            // Poll RDSR until WIP clears (bit 0)
            // Use a simple retry counter instead of Instant (not available on WASM)
            let max_polls = 1000;
            for poll in 0..max_polls {
                let status = self.spi_transceive(&[opcodes::RDSR], 1).await?;
                if status[0] & 0x01 == 0 {
                    break;
                }
                if poll == max_polls - 1 {
                    return Err(DediprogError::Timeout);
                }
                platform_sleep!(Duration::from_micros(100));
            }

            offset += chunk_len;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OpaqueMaster trait implementation
// ---------------------------------------------------------------------------

#[maybe_async(AFIT)]
impl OpaqueMaster for Dediprog {
    fn size(&self) -> usize {
        self.flash_size.unwrap_or(0) as usize
    }

    fn set_read_op(&mut self, op: SpiReadOp) {
        log::debug!(
            "Dediprog: set_read_op opcode=0x{:02X} io_mode={:?} dummy={} native_4ba={}",
            op.opcode,
            op.io_mode,
            op.dummy_cycles,
            op.native_4ba
        );
        self.selected_read_op = Some(op);
    }

    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }

        // Split into: head residue + aligned bulk + tail residue
        let chunk_size = BULK_CHUNK_SIZE;
        let head_residue = if !(addr as usize).is_multiple_of(chunk_size) {
            len.min(chunk_size - (addr as usize % chunk_size))
        } else {
            0
        };

        // Head: slow read for unaligned start
        if head_residue > 0 {
            self.slow_read(addr, &mut buf[..head_residue])
                .await
                .map_err(|_| CoreError::ReadError { addr })?;
        }

        // Aligned bulk portion
        let bulk_start = addr + head_residue as u32;
        let remaining = len - head_residue;
        let bulk_len = (remaining / chunk_size) * chunk_size;

        if bulk_len > 0 {
            // Split into chunks that fit in a single USB buffer.
            let max_blocks = (MAX_BLOCK_COUNT as usize).min(MAX_READ_BLOCKS);
            let mut bulk_offset = 0usize;
            while bulk_offset < bulk_len {
                let this_len = (bulk_len - bulk_offset).min(max_blocks * chunk_size);
                let this_len = (this_len / chunk_size) * chunk_size;
                if this_len == 0 {
                    break;
                }
                let buf_start = head_residue + bulk_offset;
                let read_addr = bulk_start + bulk_offset as u32;
                self.bulk_read_flash(read_addr, &mut buf[buf_start..buf_start + this_len])
                    .await
                    .map_err(|_| CoreError::ReadError { addr: read_addr })?;
                bulk_offset += this_len;
            }
        }

        // Tail: slow read for remaining bytes
        let tail_start = head_residue + bulk_len;
        if tail_start < len {
            let tail_addr = addr + tail_start as u32;
            self.slow_read(tail_addr, &mut buf[tail_start..])
                .await
                .map_err(|_| CoreError::ReadError { addr: tail_addr })?;
        }

        Ok(())
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        const PAGE_SIZE: usize = 256;
        let len = data.len();
        if len == 0 {
            return Ok(());
        }

        // Split into: head residue + aligned bulk + tail residue
        // Bulk writes require 256-byte (page) alignment
        let head_residue = if !(addr as usize).is_multiple_of(PAGE_SIZE) {
            len.min(PAGE_SIZE - (addr as usize % PAGE_SIZE))
        } else {
            0
        };

        // Head: slow write for unaligned start
        if head_residue > 0 {
            self.slow_write(addr, &data[..head_residue])
                .await
                .map_err(|_| CoreError::WriteError { addr })?;
        }

        // Aligned bulk portion
        let bulk_start = addr + head_residue as u32;
        let remaining = len - head_residue;
        let bulk_len = (remaining / PAGE_SIZE) * PAGE_SIZE;

        if bulk_len > 0 {
            // Split into chunks that fit in a single USB buffer.
            // Each page is 512 bytes on the wire (256 data + 256 padding), so
            // MAX_WRITE_PAGES pages = MAX_WRITE_PAGES * 512 bytes USB buffer.
            let max_pages = (MAX_BLOCK_COUNT as usize).min(MAX_WRITE_PAGES);
            let mut bulk_offset = 0usize;
            while bulk_offset < bulk_len {
                let this_len = (bulk_len - bulk_offset).min(max_pages * PAGE_SIZE);
                let this_len = (this_len / PAGE_SIZE) * PAGE_SIZE;
                if this_len == 0 {
                    break;
                }
                let data_start = head_residue + bulk_offset;
                let write_addr = bulk_start + bulk_offset as u32;
                self.bulk_write_flash(write_addr, &data[data_start..data_start + this_len])
                    .await
                    .map_err(|_| CoreError::WriteError { addr: write_addr })?;
                bulk_offset += this_len;
            }
        }

        // Tail: slow write for remaining bytes
        let tail_start = head_residue + bulk_len;
        if tail_start < len {
            let tail_addr = addr + tail_start as u32;
            self.slow_write(tail_addr, &data[tail_start..])
                .await
                .map_err(|_| CoreError::WriteError { addr: tail_addr })?;
        }

        Ok(())
    }

    async fn erase(&mut self, _addr: u32, _len: u32) -> CoreResult<()> {
        // Erase is not supported through the opaque path.
        // The HybridFlashDevice adapter uses SpiMaster for erase operations,
        // since the Dediprog firmware has no bulk erase command.
        Err(CoreError::ProgrammerError)
    }
}

// ---------------------------------------------------------------------------
// SpiMaster trait implementation
// ---------------------------------------------------------------------------

#[maybe_async(AFIT)]
impl SpiMaster for Dediprog {
    fn features(&self) -> SpiFeatures {
        let mut features = SpiFeatures::empty();

        // 4BA support depends on protocol version
        if self.protocol >= Protocol::V2 {
            features |= SpiFeatures::FOUR_BYTE_ADDR;
        }

        // Multi-I/O support for SF600 class with protocol V2+.
        // Cap by `max_io_mode` (from IoModePolicy::Auto defaults to Quad, or
        // IoModePolicy::Force(X) caps at X).
        if self.device_type.is_sf600_class() && self.protocol >= Protocol::V2 {
            // Dual output (1-1-2) is safe on V2+ for SF600 class.
            if self.max_io_mode >= DpIoMode::DualOut {
                features |= SpiFeatures::DUAL_IN;
            }
            // Dual I/O (1-2-2) is V3+ only (V2 has dummy-cycle bugs per flashprog).
            if self.max_io_mode >= DpIoMode::DualIo && self.protocol >= Protocol::V3 {
                features |= SpiFeatures::DUAL_IO;
            }
            // Quad output (1-1-4)
            if self.max_io_mode >= DpIoMode::QuadOut {
                features |= SpiFeatures::QUAD_IN;
            }
            // Quad I/O (1-4-4) is V3+ only
            if self.max_io_mode >= DpIoMode::QuadIo && self.protocol >= Protocol::V3 {
                features |= SpiFeatures::QUAD_IO;
            }
            // QPI (4-4-4) requires V3+ and explicit Qpi configuration
            if self.max_io_mode >= DpIoMode::Qpi && self.protocol >= Protocol::V3 {
                features |= SpiFeatures::QPI;
            }
        }

        // Some protocol versions have restrictions on 4BA modes
        if self.protocol == Protocol::V1
            && (self.device_type == DeviceType::SF100 || self.device_type.is_sf600_class())
        {
            // V1 on SF100 or SF600 class doesn't have 4BA mode restrictions
        } else if self.protocol < Protocol::V2 {
            features |= SpiFeatures::NO_4BA_MODES;
        }

        features
    }

    fn max_read_len(&self) -> usize {
        // Maximum data read in a single transceive command
        16
    }

    fn max_write_len(&self) -> usize {
        // Maximum data write in a single transceive command (minus 5 for cmd/addr)
        16 - 5
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check I/O mode support against advertised master features first.
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // The dediprog's generic `spi_transceive` command is single-IO only
        // regardless of what `features()` advertises (multi-IO is reachable
        // only via `OpaqueMaster::read` / `CMD_READ`). Reject multi-IO here
        // loudly rather than silently downgrading and producing wrong data.
        if cmd.io_mode != CoreIoMode::Single {
            log::error!(
                "Dediprog::execute called with io_mode={:?}; only Single is \
                 supported via the generic SPI transceive path. Multi-IO \
                 reads must go through OpaqueMaster::read.",
                cmd.io_mode,
            );
            return Err(CoreError::ProgrammerError);
        }

        // For simple commands, use transceive
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];
        cmd.encode_header(&mut write_data);
        write_data[header_len..].copy_from_slice(cmd.write_data);

        let read_len = cmd.read_buf.len();
        let result = self
            .spi_transceive(&write_data, read_len)
            .await
            .map_err(|_e| CoreError::ProgrammerError)?;

        cmd.read_buf
            .copy_from_slice(&result[..read_len.min(result.len())]);

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        platform_sleep!(Duration::from_micros(us as u64));
    }
}
