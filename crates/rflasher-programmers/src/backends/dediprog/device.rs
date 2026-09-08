//! Dediprog device implementation
//!
//! This module provides the main `Dediprog` struct that implements USB
//! communication with Dediprog SF100/SF200/SF600/SF700 programmers.
//!
//! Async on every target: native drives the async API with a `block_on`
//! boundary in the application, WASM uses WebUSB.

use std::time::Duration;

use nusb::Endpoint;
use nusb::transfer::{Buffer, Bulk, In, Out};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::flash::io::{self, AddressPlan, ReadPlan, WritePlan};
use rflasher_core::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use rflasher_core::protocol::SpiReadOp;
use rflasher_core::spi::{IoMode as CoreIoMode, SpiCommand, check_io_mode_supported, opcodes};

use super::error::{DediprogError, Result};
use super::protocol::*;
use crate::usb_ep::EpWaitExt;

// ---------------------------------------------------------------------------
// Platform-specific endpoint wait macros
// ---------------------------------------------------------------------------
// These macros provide a uniform interface over nusb's blocking (native)
// and async (WASM) completion APIs.

/// Wait for the next completion on an endpoint, giving up after the timeout.
/// Returns `Option<Completion>` (`None` on timeout).
macro_rules! ep_wait {
    ($ep:expr, $timeout:expr) => {
        $ep.next_complete_timeout($timeout).await
    };
}

/// Resolve an nusb `MaybeFuture` to its output.
/// In sync mode: calls `.await` (blocking).
/// In async mode: `.await`s the future.
macro_rules! nusb_await {
    ($expr:expr) => {{ $expr.await }};
}

/// Platform-aware sleep/delay.
/// In sync mode: std::thread::sleep.
/// In async mode (WASM): setTimeout-based delay.
macro_rules! platform_sleep {
    ($dur:expr) => {{
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::thread::sleep($dur);
        }
        #[cfg(target_arch = "wasm32")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IoModePolicy {
    /// Let the flash layer decide based on chip + programmer capabilities.
    #[default]
    Auto,
    /// Force a specific upper bound.
    Force(DpIoMode),
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

#[cfg(all(feature = "std", not(feature = "wasm")))]
impl Dediprog {
    /// Open the first available Dediprog device
    pub async fn open() -> Result<Self> {
        Self::open_with_config(DediprogConfig::default()).await
    }

    /// Open a Dediprog device with the specified configuration
    pub async fn open_with_config(config: DediprogConfig) -> Result<Self> {
        // Find matching devices
        let devices: Vec<_> = nusb::list_devices()
            .await
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
                match Self::try_open_device(device_info, &config).await {
                    Ok(mut dediprog) => {
                        // Read device ID and check
                        if let Ok(id) = dediprog.read_device_id().await {
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

        Self::try_open_device(device_info, &config).await
    }

    /// Try to open a specific USB device (native/blocking)
    async fn try_open_device(
        device_info: &nusb::DeviceInfo,
        config: &DediprogConfig,
    ) -> Result<Self> {
        log::info!(
            "Opening Dediprog at bus {} address {}",
            device_info.bus_id(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .await
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?;

        // Claim interface 0
        let interface = device
            .claim_interface(0)
            .await
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
        };

        dediprog.init_device(config).await?;
        Ok(dediprog)
    }

    /// List all connected Dediprog devices
    pub async fn list_devices() -> Result<Vec<DediprogDeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .await
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?
            .filter(|d| {
                d.vendor_id() == DEDIPROG_USB_VENDOR && d.product_id() == DEDIPROG_USB_PRODUCT
            })
            .map(|d| DediprogDeviceInfo {
                bus_id: d.bus_id().to_string(),
                address: d.device_address(),
            })
            .collect();

        Ok(devices)
    }
}

/// Information about a connected Dediprog device
#[cfg(all(feature = "std", not(feature = "wasm")))]
#[derive(Debug, Clone)]
pub struct DediprogDeviceInfo {
    /// USB bus identifier (platform-defined; integer string on Linux)
    pub bus_id: String,
    /// USB device address
    pub address: u8,
}

#[cfg(all(feature = "std", not(feature = "wasm")))]
impl std::fmt::Display for DediprogDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Dediprog at bus {} address {}",
            self.bus_id, self.address
        )
    }
}

// Native-only best-effort cleanup. On WASM, dropping mid-operation from a
// cancelled task must not block the event loop; explicit shutdown is used
// there instead.
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Dediprog {
    fn drop(&mut self) {
        futures_lite::future::block_on(async {
            // Reset I/O mode
            let _ = self.set_io_mode(DpIoMode::Single).await;
            // Turn off voltage
            let _ = self.set_voltage(0).await;
        });
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, async open, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl Dediprog {
    /// Request a Dediprog device via the WebUSB permission prompt
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    /// It shows the browser's device picker filtered to Dediprog devices.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        log::info!("Requesting Dediprog device via WebUSB picker...");

        let selector =
            nusb::DeviceSelector::all().with_vid_pid(DEDIPROG_USB_VENDOR, DEDIPROG_USB_PRODUCT);
        let device_info = nusb::request_device(&[selector])
            .await
            .map_err(|e| DediprogError::OpenFailed(format!("WebUSB request failed: {e}")))?
            .ok_or(DediprogError::DeviceNotFound)?;

        log::info!(
            "Dediprog device selected: VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

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
// Shared methods (async on every target)
// ---------------------------------------------------------------------------

// When all features are enabled simultaneously (e.g. --all-features in CI),
// the mutually-exclusive open() methods are both excluded, making these shared
// helpers appear unused. Allow dead_code for that configuration.
#[cfg_attr(any(), allow(dead_code))]
impl Dediprog {
    /// Initialize device after USB connection is established.
    /// Shared between native and WASM paths.
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

        // Determine multi-I/O support. Mirror flashprog's default: multi-I/O
        // is only enabled by default on SF600Plus-G2 (dual); every other
        // model defaults to single-I/O and opts in explicitly via
        // iomode=dual/quad ("Multi i/o is only tested with SF600Plus-G2,
        // not enabling by default").
        if self.device_type.is_sf600_class() && self.protocol >= Protocol::V2 {
            self.max_io_mode = match self.io_mode_policy {
                IoModePolicy::Auto => {
                    if self.device_type == DeviceType::SF600PG2 {
                        DpIoMode::DualIo
                    } else {
                        DpIoMode::Single
                    }
                }
                IoModePolicy::Force(m) => m,
            };
        } else {
            self.max_io_mode = DpIoMode::Single;
        }

        self.set_leds(Led::None).await?;

        Ok(())
    }

    /// Read the device string and parse device type/firmware
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
    async fn set_target(&mut self, target: Target) -> Result<()> {
        self.control_write(Command::SetTarget, target as u16, 0, &[])
            .await?;
        Ok(())
    }

    /// Set the SPI clock speed
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
        #[cfg(not(target_arch = "wasm32"))]
        let recipient = if request_type & 0x03 == 0x02 {
            nusb::transfer::Recipient::Endpoint
        } else {
            nusb::transfer::Recipient::Other
        };
        #[cfg(target_arch = "wasm32")]
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
    async fn control_write_raw(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        // See control_read_raw for rationale on the recipient override.
        #[cfg(not(target_arch = "wasm32"))]
        let recipient = nusb::transfer::Recipient::Endpoint;
        #[cfg(target_arch = "wasm32")]
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
            #[cfg(not(target_arch = "wasm32"))]
            let recipient = nusb::transfer::Recipient::Endpoint;
            #[cfg(target_arch = "wasm32")]
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

            if data.len() != to_read {
                return Err(DediprogError::TransferFailed(format!(
                    "Short SPI response: expected {to_read} bytes, got {}",
                    data.len()
                )));
            }
            buf[total_read..total_read + to_read].copy_from_slice(&data);
            total_read += to_read;
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

    async fn select_extended_address(&mut self, plan: AddressPlan, address: u32) -> Result<()> {
        if let AddressPlan::Ear(features) = plan {
            rflasher_core::protocol::set_extended_address(self, features, (address >> 24) as u8)
                .await?;
        }
        Ok(())
    }

    /// Bulk read from flash using CMD_READ + USB bulk IN transfers.
    ///
    /// Start and len MUST be 512-byte aligned. Uses a single large URB so the
    /// kernel handles all USB scheduling internally -- avoids per-packet
    /// userspace round-trips through nusb's epoll background thread.
    async fn bulk_read_flash(&mut self, plan: &ReadPlan, start: u32, buf: &mut [u8]) -> Result<()> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }

        let count = (len / BULK_CHUNK_SIZE) as u16;

        let (cmd_buf, cmd_len, value, idx) = read_packet(self.protocol, plan, start, count)?;
        self.select_extended_address(plan.address, start).await?;
        let dp_mode = DpIoMode::from(plan.op.io_mode);
        self.set_io_mode(dp_mode).await?;

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
        let timeout =
            Duration::from_secs(ASYNC_TIMEOUT_SECS) + Duration::from_micros(len as u64 * 30);

        let result = ep_wait!(in_ep, timeout).ok_or(DediprogError::Timeout)?;
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
    async fn bulk_write_flash(&mut self, plan: &WritePlan, start: u32, data: &[u8]) -> Result<()> {
        const PAGE_SIZE: usize = 256;
        let len = data.len();
        if len == 0 {
            return Ok(());
        }

        self.select_extended_address(plan.address, start).await?;
        let count = (len / PAGE_SIZE) as u16;

        // Writes always use single I/O
        self.set_io_mode(DpIoMode::Single).await?;

        let (cmd_buf, cmd_len, value, idx) = write_packet(self.protocol, plan, start, count)?;

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

        fill_write_buffer(data, &mut out_buf);

        out_ep.submit(out_buf);

        // Scale timeout with transfer size: 10 s base + 10 ms per page
        // (accommodates worst-case page-program time of typical NOR flash)
        let timeout =
            Duration::from_secs(ASYNC_TIMEOUT_SECS) + Duration::from_millis(count as u64 * 10);

        let result = ep_wait!(out_ep, timeout).ok_or(DediprogError::Timeout)?;
        result
            .status
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;
        if result.actual_len != total_usb_len {
            return Err(DediprogError::TransferFailed("Short bulk write".into()));
        }
        Ok(())
    }

    /// Residuals use the plan's single-I/O operation, never guessed addressing.
    async fn slow_read(&mut self, plan: &ReadPlan, addr: u32, buf: &mut [u8]) -> Result<()> {
        let mut residual = *plan;
        residual.op = plan.single_read.ok_or(CoreError::ChipNotSupported)?;
        io::read_spi(self, &residual, addr, buf).await?;
        Ok(())
    }
    async fn slow_write(&mut self, plan: &WritePlan, addr: u32, data: &[u8]) -> Result<()> {
        io::write_spi(self, plan, addr, data).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OpaqueMaster trait implementation
// ---------------------------------------------------------------------------

impl OpaqueMaster for Dediprog {
    fn size(&self) -> usize {
        self.flash_size.unwrap_or(0) as usize
    }

    fn supports_read_plan(&self, plan: &ReadPlan) -> bool {
        read_plan_supported(self.protocol, plan)
            && check_io_mode_supported(plan.op.io_mode, self.features()).is_ok()
    }
    fn supports_write_plan(&self, plan: &WritePlan) -> bool {
        write_plan_supported(self.protocol, plan)
    }
    // This programmer is not genuinely opaque: chip metadata is mandatory.
    async fn read(&mut self, _addr: u32, _buf: &mut [u8]) -> CoreResult<()> {
        Err(CoreError::ChipNotSupported)
    }
    async fn write(&mut self, _addr: u32, _data: &[u8]) -> CoreResult<()> {
        Err(CoreError::ChipNotSupported)
    }

    async fn read_planned(&mut self, plan: &ReadPlan, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
        if !self.supports_read_plan(plan) {
            return Err(CoreError::ChipNotSupported);
        }
        plan.address.validate_range(addr, buf.len())?;
        let mut offset = 0;
        while offset < buf.len() {
            let address = addr + offset as u32;
            let (count, bulk) = transfer_chunk(
                address,
                buf.len() - offset,
                BULK_CHUNK_SIZE,
                (MAX_BLOCK_COUNT as usize).min(MAX_READ_BLOCKS) * BULK_CHUNK_SIZE,
            );
            let chunk = &mut buf[offset..offset + count];
            if bulk {
                self.bulk_read_flash(plan, address, chunk).await
            } else {
                self.slow_read(plan, address, chunk).await
            }
            .map_err(|_| CoreError::ReadError { addr: address })?;
            offset += count;
        }
        Ok(())
    }

    async fn write_planned(&mut self, plan: &WritePlan, addr: u32, data: &[u8]) -> CoreResult<()> {
        if !self.supports_write_plan(plan) {
            return Err(CoreError::ChipNotSupported);
        }
        plan.address.validate_range(addr, data.len())?;
        let mut offset = 0;
        while offset < data.len() {
            let address = addr + offset as u32;
            let (count, bulk) = transfer_chunk(
                address,
                data.len() - offset,
                256,
                (MAX_BLOCK_COUNT as usize).min(MAX_WRITE_PAGES) * 256,
            );
            let chunk = &data[offset..offset + count];
            if bulk {
                self.bulk_write_flash(plan, address, chunk).await
            } else {
                self.slow_write(plan, address, chunk).await
            }
            .map_err(|_| CoreError::WriteError { addr: address })?;
            offset += count;
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

impl SpiMaster for Dediprog {
    fn features(&self) -> SpiFeatures {
        let mut features = SpiFeatures::empty();

        // 4BA support depends on protocol version
        if self.protocol >= Protocol::V2 {
            features |= SpiFeatures::FOUR_BYTE_ADDR;
        }

        // Multi-I/O support for SF600 class with protocol V2+.
        // Cap by `max_io_mode` (from IoModePolicy::Auto, which mirrors
        // flashprog: single everywhere except dual on SF600Plus-G2 — or
        // IoModePolicy::Force(X) which caps at X).
        if self.device_type.is_sf600_class() && self.protocol >= Protocol::V2 {
            // Dual output (1-1-2, 0x3B) works on V2+: the V2 read packet
            // passes the opcode through under ReadMode::Fast with
            // firmware-side timing.
            if self.max_io_mode >= DpIoMode::DualOut && self.protocol >= Protocol::V2 {
                features |= SpiFeatures::DUAL_IN;
            }
            // Dual I/O (1-2-2, 0xBB) is V3+ only: the V2 fixed-op form uses
            // the wrong number of dummy cycles (flashprog
            // dediprog_init: "The v2, fixed-op JEDEC_FAST_READ_DUAL_DIO
            // command seems to use the wrong number of dummy cycles").
            if self.max_io_mode >= DpIoMode::DualIo && self.protocol >= Protocol::V3 {
                features |= SpiFeatures::DUAL_IO;
            }
            // Quad output (1-1-4) is V3+ only (same V2 dummy-cycle limitation).
            if self.max_io_mode >= DpIoMode::QuadOut && self.protocol >= Protocol::V3 {
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
        let supports_4ba_modes = (self.device_type == DeviceType::SF100
            && self.protocol == Protocol::V1)
            || (self.device_type.is_sf600_class() && self.protocol == Protocol::V3);
        if !supports_4ba_modes {
            features |= SpiFeatures::NO_4BA_MODES;
        }

        features
    }

    fn supports_read_dummy_cycles(&self, _mode: CoreIoMode, cycles: u8) -> bool {
        read_dummy_supported(self.protocol, cycles)
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

        if !rflasher_core::spi::dummy_cycles_representable(cmd.io_mode, cmd.dummy_cycles) {
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

        if result.len() != read_len {
            return Err(CoreError::SpiTransferFailed);
        }
        cmd.read_buf.copy_from_slice(&result);

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        platform_sleep!(Duration::from_micros(us as u64));
    }
}

// The same split is used by the actual bulk IN/OUT paths and packet tests.
fn transfer_chunk(addr: u32, remaining: usize, alignment: usize, max_bulk: usize) -> (usize, bool) {
    let available = remaining.min(0x0100_0000 - (addr as usize & 0xffffff));
    let misalignment = addr as usize % alignment;
    if misalignment != 0 {
        (available.min(alignment - misalignment), false)
    } else if available < alignment {
        (available, false)
    } else {
        (available.min(max_bulk) / alignment * alignment, true)
    }
}
fn fill_write_buffer(data: &[u8], buffer: &mut Buffer) {
    for page in data.chunks_exact(256) {
        buffer.extend_from_slice(page);
        buffer.extend_from_slice(&[0xff; BULK_CHUNK_SIZE - 256]);
    }
}

fn read_plan_supported(protocol: Protocol, plan: &ReadPlan) -> bool {
    let op = plan.op;
    if op.address_width != plan.address.width()
        || op.native_4ba != (plan.address == AddressPlan::NativeFourByte)
        || !read_dummy_supported(protocol, op.dummy_cycles)
    {
        return false;
    }
    let Some(single) = plan.single_read else {
        return false;
    };
    if single.io_mode != CoreIoMode::Single
        || single.address_width != plan.address.width()
        || single.native_4ba != op.native_4ba
        || !single.dummy_cycles.is_multiple_of(8)
    {
        return false;
    }
    match protocol {
        Protocol::V1 => {
            op.opcode == opcodes::READ
                && op.io_mode == CoreIoMode::Single
                && op.dummy_cycles == 0
                && op.address_width.bytes() == 3
        }
        // V2 cannot express compatibility-four-byte framing. Its implicit 0x13
        // -> 0x0c substitution is not safe unless chip metadata authorizes 0x0c;
        // selection instead supplies an explicit native fast-read plan.
        Protocol::V2 => {
            !matches!(plan.address, AddressPlan::EnterFourByte(_))
                && matches!(
                    (op.opcode, op.io_mode, op.dummy_cycles),
                    (0x03, CoreIoMode::Single, 0)
                        | (0x0b | 0x0c, CoreIoMode::Single, 8)
                        | (0x3b | 0x3c, CoreIoMode::DualOut, 8)
                )
        }
        Protocol::V3 => op.io_mode != CoreIoMode::Qpi,
        Protocol::Unknown => false,
    }
}
fn write_plan_supported(protocol: Protocol, plan: &WritePlan) -> bool {
    use rflasher_core::chip::WriteGranularity;
    if plan.page_size != 256
        || plan.granularity != WriteGranularity::Page
        || plan.opcode
            != if plan.address == AddressPlan::NativeFourByte {
                opcodes::PP_4B
            } else {
                opcodes::PP
            }
    {
        return false;
    }
    match protocol {
        Protocol::V1 => plan.address.width().bytes() == 3,
        Protocol::V2 => !matches!(plan.address, AddressPlan::EnterFourByte(_)),
        Protocol::V3 => true,
        Protocol::Unknown => false,
    }
}

fn packet_header(
    protocol: Protocol,
    start: u32,
    count: u16,
    mode: u8,
) -> ([u8; MAX_CMD_SIZE], u16, u16) {
    let mut packet = [0; MAX_CMD_SIZE];
    packet[..2].copy_from_slice(&count.to_le_bytes());
    packet[3] = mode;
    if protocol == Protocol::V1 {
        (packet, start as u16, ((start >> 16) & 0xff) as u16)
    } else {
        packet[6..10].copy_from_slice(&start.to_le_bytes());
        (packet, 0, 0)
    }
}
fn read_packet(
    protocol: Protocol,
    plan: &ReadPlan,
    start: u32,
    count: u16,
) -> Result<([u8; MAX_CMD_SIZE], usize, u16, u16)> {
    if !read_plan_supported(protocol, plan) {
        return Err(DediprogError::Unsupported(
            "Unrepresentable read plan".into(),
        ));
    }
    let (mut packet, value, index) = packet_header(protocol, start, count, ReadMode::Std as u8);
    encode_read_op(protocol, plan.op, &mut packet)?;
    let len = match protocol {
        Protocol::V1 => 5,
        Protocol::V2 => 10,
        _ => 12,
    };
    Ok((packet, len, value, index))
}
fn write_packet(
    protocol: Protocol,
    plan: &WritePlan,
    start: u32,
    count: u16,
) -> Result<([u8; MAX_CMD_SIZE], usize, u16, u16)> {
    if !write_plan_supported(protocol, plan) {
        return Err(DediprogError::Unsupported(
            "Unrepresentable write plan".into(),
        ));
    }
    let (mut packet, value, index) =
        packet_header(protocol, start, count, WriteMode::PagePgm as u8);
    if protocol != Protocol::V1 {
        packet[4] = plan.opcode;
        if plan.address.width().bytes() == 4 {
            packet[3] = if protocol == Protocol::V2 {
                WriteMode::FourByteAddr256BPagePgm0x12 as u8
            } else {
                WriteMode::FourByteAddr256BPagePgm as u8
            };
        }
    }
    if protocol == Protocol::V3 {
        packet[10..14].copy_from_slice(&(plan.page_size as u32).to_le_bytes());
    }
    let len = match protocol {
        Protocol::V1 => 5,
        Protocol::V2 => 10,
        _ => 14,
    };
    Ok((packet, len, value, index))
}

// V2 firmware has fixed fast-read latency. V3 expresses pairs of clocks.
fn read_dummy_supported(protocol: Protocol, cycles: u8) -> bool {
    match protocol {
        Protocol::V3 => cycles.is_multiple_of(2),
        _ => cycles == 0 || cycles == 8,
    }
}

fn encode_read_op(
    protocol: Protocol,
    op: SpiReadOp,
    packet: &mut [u8; MAX_CMD_SIZE],
) -> Result<()> {
    if !read_dummy_supported(protocol, op.dummy_cycles) {
        return Err(DediprogError::Unsupported(
            "Unrepresentable read dummy clocks".into(),
        ));
    }
    if protocol == Protocol::V2 {
        packet[3] = if op.native_4ba {
            ReadMode::FourByteAddrFast0x0C as u8
        } else if op.opcode != opcodes::READ {
            ReadMode::Fast as u8
        } else {
            ReadMode::Std as u8
        };
        packet[4] = op.opcode;
    } else if protocol == Protocol::V3 {
        packet[3] = ReadMode::Configurable as u8;
        packet[4] = op.opcode;
        packet[10] = op.address_width.bytes();
        packet[11] = op.dummy_cycles / 2;
    }
    Ok(())
}

#[cfg(test)]
mod framing_tests {
    use super::*;
    use rflasher_core::spi::AddressWidth;

    use rflasher_core::chip::{Features, WriteGranularity};
    use rflasher_core::flash::FlashContext;
    struct PlanningMaster;
    impl SpiMaster for PlanningMaster {
        fn features(&self) -> SpiFeatures {
            SpiFeatures::FOUR_BYTE_ADDR
                | SpiFeatures::DUAL_IN
                | SpiFeatures::DUAL_IO
                | SpiFeatures::QUAD_IO
        }
        fn max_read_len(&self) -> usize {
            16
        }
        fn max_write_len(&self) -> usize {
            11
        }
        async fn execute(&mut self, _: &mut SpiCommand<'_>) -> CoreResult<()> {
            panic!("selection did I/O")
        }
        async fn delay_us(&mut self, _: u32) {}
    }
    fn context(features: Features, size: u32) -> FlashContext {
        use rflasher_core::sfdp::*;
        let info = SfdpInfo {
            header: SfdpHeader::parse(&[0x53, 0x46, 0x44, 0x50, 6, 1, 0, 0xff]),
            basic_params: BasicFlashParams {
                density_bytes: size.into(),
                page_size: 256,
                ..Default::default()
            },
            num_param_headers: 1,
            four_byte_addr_table: None,
        };
        let mut chip = to_flash_chip(&info, 0, 0);
        chip.features = features;
        chip.write_granularity = WriteGranularity::Page;
        FlashContext::new(chip)
    }
    #[test]
    fn read_plans_cover_v1_v2_v3_bulk_packets_and_exact_residuals() {
        let features = Features::FAST_READ
            | Features::FAST_READ_DOUT
            | Features::FOUR_BYTE_DUAL_OUT_READ
            | Features::FOUR_BYTE_READ
            | Features::FOUR_BYTE_ENTER;
        for (protocol, size, opcode, mode) in [
            (Protocol::V1, 1 << 20, 0x03, ReadMode::Std),
            (Protocol::V2, 1 << 20, 0x3b, ReadMode::Fast),
            (Protocol::V2, 32 << 20, 0x3c, ReadMode::FourByteAddrFast0x0C),
            (Protocol::V3, 32 << 20, 0x3c, ReadMode::Configurable),
        ] {
            let ctx = context(features, size);
            let plan = io::select_read_plan(&PlanningMaster, &ctx, false, |p| {
                read_plan_supported(protocol, p)
            })
            .unwrap();
            let (packet, len, value, index) = read_packet(protocol, &plan, 0x123400, 8).unwrap();
            assert_eq!(plan.op.opcode, opcode);
            assert_eq!(packet[3], mode as u8);
            assert_eq!(&packet[..2], &[8, 0]);
            assert_eq!(
                plan.single_read.unwrap().opcode,
                if size > 16 << 20 { 0x13 } else { 0x03 }
            );
            if protocol == Protocol::V1 {
                assert_eq!((len, value, index), (5, 0x3400, 0x12));
            } else {
                assert_eq!(packet[4], opcode);
                assert_eq!(&packet[6..10], &0x123400u32.to_le_bytes());
            }
        }
        // Native READ alone must never authorize firmware's implicit FAST_READ.
        let ctx = context(Features::FOUR_BYTE_READ, 32 << 20);
        assert!(
            io::select_read_plan(&PlanningMaster, &ctx, false, |p| read_plan_supported(
                Protocol::V2,
                p
            ))
            .is_err()
        );
        let spi = io::select_read_plan(&PlanningMaster, &ctx, true, |_| true).unwrap();
        assert_eq!(spi.op.opcode, 0x13); // safe pre-I/O generic read fallback
    }
    #[test]
    fn program_addressing_is_independent_and_v2_chooses_native_or_ear_not_en4b() {
        for protocol in [Protocol::V1, Protocol::V2, Protocol::V3] {
            let ctx = context(
                Features::FOUR_BYTE_PROGRAM
                    | Features::FOUR_BYTE_ENTER
                    | Features::EXT_ADDR_REG_C5C8,
                32 << 20,
            );
            let plan =
                io::select_write_plan(&PlanningMaster, &ctx, |p| write_plan_supported(protocol, p))
                    .unwrap();
            assert_eq!(
                plan.opcode,
                if protocol == Protocol::V1 { 0x02 } else { 0x12 }
            );
            let (packet, _, _, _) = write_packet(protocol, &plan, 0x0100_0000, 2).unwrap();
            assert_eq!(&packet[..2], &[2, 0]);
            if protocol == Protocol::V2 {
                assert_eq!(packet[3], WriteMode::FourByteAddr256BPagePgm0x12 as u8);
            }
            if protocol == Protocol::V3 {
                assert_eq!(packet[3], WriteMode::FourByteAddr256BPagePgm as u8);
                assert_eq!(&packet[10..14], &256u32.to_le_bytes());
            }
        }
        let ctx = context(Features::FOUR_BYTE_ENTER, 32 << 20);
        assert!(
            io::select_write_plan(&PlanningMaster, &ctx, |p| write_plan_supported(
                Protocol::V2,
                p
            ))
            .is_err()
        );
        let plan = io::select_write_plan(&PlanningMaster, &ctx, |p| {
            write_plan_supported(Protocol::V3, p)
        })
        .unwrap();
        assert!(matches!(plan.address, AddressPlan::EnterFourByte(_)));
        assert_eq!(
            &write_packet(Protocol::V3, &plan, 0x0100_0000, 1).unwrap().0[3..5],
            &[WriteMode::FourByteAddr256BPagePgm as u8, 0x02]
        );
        let ctx = context(
            Features::FOUR_BYTE_ENTER | Features::EXT_ADDR_REG_C5C8,
            32 << 20,
        );
        let plan = io::select_write_plan(&PlanningMaster, &ctx, |p| {
            write_plan_supported(Protocol::V2, p)
        })
        .unwrap();
        assert!(matches!(plan.address, AddressPlan::Ear(_)));
    }
    #[test]
    fn aligned_writes_route_to_padded_bulk_out_and_splits_keep_every_byte_at_bank_edges() {
        for alignment in [256, 512] {
            let start = 0x00ff_fc03u32;
            let len = 4000;
            let mut offset = 0;
            let mut bulk_count = 0;
            let mut residual_count = 0;
            while offset < len {
                let address = start + offset as u32;
                let (count, bulk) = transfer_chunk(address, len - offset, alignment, 1024);
                assert!(count > 0);
                assert!((address & 0xffffff) as usize + count <= 0x0100_0000);
                if bulk {
                    bulk_count += 1;
                    assert!(count.is_multiple_of(alignment));
                    assert!((address as usize).is_multiple_of(alignment));
                } else {
                    residual_count += 1;
                }
                offset += count;
            }
            assert_eq!(offset, len);
            assert!(bulk_count >= 3);
            assert_eq!(residual_count, 2);
        }
        let (count, bulk) = transfer_chunk(0x0100_0000, 512, 256, MAX_WRITE_PAGES * 256);
        assert_eq!((count, bulk), (512, true));
        let mut buffer = Buffer::new(1024);
        fill_write_buffer(&[0xa5; 512], &mut buffer);
        assert_eq!(buffer.len(), 1024);
        for page in buffer.chunks_exact(512) {
            assert_eq!(&page[..256], &[0xa5; 256]);
            assert_eq!(&page[256..], &[0xff; 256]);
        }
    }

    #[test]
    fn v2_native_address_mode_is_independent_of_opcode() {
        for (opcode, native, mode, wire_opcode) in [
            (0x3c, true, ReadMode::FourByteAddrFast0x0C, 0x3c),
            (0x3b, false, ReadMode::Fast, 0x3b),
        ] {
            let mut packet = [0; MAX_CMD_SIZE];
            let op = SpiReadOp {
                opcode,
                io_mode: CoreIoMode::DualOut,
                dummy_cycles: 8,
                address_width: if native {
                    AddressWidth::FourByte
                } else {
                    AddressWidth::ThreeByte
                },
                native_4ba: native,
            };
            encode_read_op(Protocol::V2, op, &mut packet).unwrap();
            assert_eq!(&packet[3..5], &[mode as u8, wire_opcode]);
        }
    }

    #[test]
    fn v3_dummy_pairs_are_exact_and_invalid_packet_is_untouched() {
        for (mode, clocks, valid) in [
            (CoreIoMode::DualIo, 5, false),
            (CoreIoMode::QuadIo, 7, false),
            (CoreIoMode::DualIo, 4, true),
            (CoreIoMode::QuadIo, 6, true),
        ] {
            let mut packet = [0; MAX_CMD_SIZE];
            let op = SpiReadOp {
                opcode: 0xeb,
                io_mode: mode,
                dummy_cycles: clocks,
                address_width: AddressWidth::FourByte,
                native_4ba: false,
            };
            assert_eq!(encode_read_op(Protocol::V3, op, &mut packet).is_ok(), valid);
            if valid {
                assert_eq!(packet[10], 4);
                assert_eq!(packet[11], clocks / 2);
            } else {
                assert_eq!(packet, [0; MAX_CMD_SIZE]);
            }
        }
    }
}
