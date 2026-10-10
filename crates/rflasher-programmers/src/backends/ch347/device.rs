//! CH347 device implementation
//!
//! This module provides the main `Ch347` struct that implements USB
//! communication with the CH347 programmer and the `SpiMaster` trait.
//!
//! Async on every target: native drives the same async code with a
//! `block_on` boundary in the application, WASM uses WebUSB.

use std::time::Duration;

use nusb::Endpoint;
use nusb::transfer::{Buffer, Bulk, In, Out};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{SpiCommand, check_io_mode_supported};

use super::error::{Ch347Error, Result};
use super::protocol::*;
use crate::usb_ep::EpWaitExt;

// ---------------------------------------------------------------------------
// Platform-specific endpoint wait macro
// ---------------------------------------------------------------------------

/// Wait for the next completion on an endpoint, giving up after the timeout.
/// Returns `Option<Completion>` (`None` on timeout).
macro_rules! ep_wait {
    ($ep:expr, $timeout:expr) => {
        $ep.next_complete_timeout($timeout).await
    };
}

// ---------------------------------------------------------------------------
// CH347 device struct
// ---------------------------------------------------------------------------

/// CH347 USB programmer
///
/// This struct represents a connection to a CH347 USB device and implements
/// the `SpiMaster` trait for communicating with SPI flash chips.
///
/// The API is async on every target; WASM uses WebUSB transfers.
pub struct Ch347 {
    /// USB interface (kept alive to maintain device claim on WASM)
    #[cfg(feature = "wasm")]
    _interface: nusb::Interface,
    /// Bulk OUT endpoint for writes
    out_ep: Endpoint<Bulk, Out>,
    /// Bulk IN endpoint for reads
    in_ep: Endpoint<Bulk, In>,
    /// Current SPI configuration
    config: SpiConfig,
    /// Device variant (T or F)
    variant: Ch347Variant,
}

// The SPI transfer orchestration is generic so error cleanup can be tested
// without opening a USB device.
trait SpiIo {
    async fn set_cs(&mut self, assert: bool) -> Result<()>;
    async fn write(&mut self, data: &[u8]) -> Result<()>;
    async fn read(&mut self, data: &mut [u8]) -> Result<()>;
}

impl SpiIo for Ch347 {
    async fn set_cs(&mut self, assert: bool) -> Result<()> {
        self.cs_control(assert).await
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.spi_write(data).await
    }

    async fn read(&mut self, data: &mut [u8]) -> Result<()> {
        self.spi_read(data).await
    }
}

async fn spi_transfer_with_io<T: SpiIo>(
    io: &mut T,
    write_data: &[u8],
    read_buf: &mut [u8],
) -> Result<()> {
    if let Err(error) = io.set_cs(true).await {
        // The assertion may have reached the device before its transfer failed.
        let _ = io.set_cs(false).await;
        return Err(error);
    }
    let result = async {
        if !write_data.is_empty() {
            io.write(write_data).await?;
        }
        if !read_buf.is_empty() {
            io.read(read_buf).await?;
        }
        Ok(())
    }
    .await;
    let deassert_result = io.set_cs(false).await;
    result.and(deassert_result)
}

fn parse_spi_read_packet(packet: &[u8], remaining: usize) -> Result<&[u8]> {
    if packet.len() < 3 {
        return Err(Ch347Error::InvalidResponse("Response too short".into()));
    }
    if packet[0] != CH347_CMD_SPI_IN {
        return Err(Ch347Error::InvalidResponse(
            "Unexpected SPI read response".into(),
        ));
    }
    let data_len = packet[1] as usize | ((packet[2] as usize) << 8);
    if data_len == 0 || packet.len() < 3 + data_len {
        return Err(Ch347Error::InvalidResponse(
            "Empty or incomplete SPI read response".into(),
        ));
    }
    Ok(&packet[3..3 + data_len.min(remaining)])
}

/// USB devices this backend drives (CH347T and CH347F).
const USB_SELECTORS: &[nusb::DeviceSelector] = &[
    nusb::DeviceSelector::all().with_vid_pid(CH347_USB_VENDOR, CH347T_USB_PRODUCT),
    nusb::DeviceSelector::all().with_vid_pid(CH347_USB_VENDOR, CH347F_USB_PRODUCT),
];

impl Ch347 {
    /// Open a CH347 device
    ///
    /// Natively, find devices with [`crate::UsbProgrammer::candidates`]; in
    /// the browser, use `request_device`.
    pub async fn open(device_info: nusb::DeviceInfo, config: SpiConfig) -> Result<Self> {
        let variant = Ch347Variant::from_product_id(device_info.product_id())
            .ok_or(Ch347Error::DeviceNotFound)?;

        log::info!(
            "Opening CH347{} device VID={:04X} PID={:04X}",
            if variant == Ch347Variant::Ch347T {
                "T"
            } else {
                "F"
            },
            device_info.vendor_id(),
            device_info.product_id()
        );

        let device = device_info
            .open()
            .await
            .map_err(|e| Ch347Error::OpenFailed(e.to_string()))?;

        // Find the vendor-specific interface for SPI
        // CH347T uses interface 2, CH347F uses interface 4
        let config_desc = device
            .active_configuration()
            .map_err(|e| Ch347Error::OpenFailed(format!("Failed to get config: {}", e)))?;

        let iface_num = find_vendor_interface(&config_desc)?;

        log::debug!("Using interface {}", iface_num);

        let interface = device
            .claim_interface(iface_num)
            .await
            .map_err(|e| Ch347Error::ClaimFailed(e.to_string()))?;

        let out_ep = interface
            .endpoint::<Bulk, Out>(WRITE_EP)
            .map_err(|e| Ch347Error::ClaimFailed(e.to_string()))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(READ_EP)
            .map_err(|e| Ch347Error::ClaimFailed(e.to_string()))?;

        let mut ch347 = Self {
            #[cfg(feature = "wasm")]
            _interface: interface,
            out_ep,
            in_ep,
            config,
            variant,
        };

        ch347.configure().await?;
        Ok(ch347)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl crate::UsbProgrammer for Ch347 {
    const NAME: &'static str = "CH347";
    type Config = SpiConfig;
    type Error = Ch347Error;

    async fn candidates(_config: &SpiConfig) -> Result<Vec<nusb::DeviceInfo>> {
        crate::usb::list_devices(USB_SELECTORS)
            .await
            .map_err(|e| Ch347Error::OpenFailed(e.to_string()))
    }

    async fn open_device(device: nusb::DeviceInfo, config: SpiConfig) -> Result<Self> {
        Self::open(device, config).await
    }
}

#[cfg(all(feature = "std", not(feature = "wasm")))]
impl Ch347 {
    /// Update SPI configuration
    ///
    /// This sends the new configuration to the device.
    pub async fn set_config(&mut self, config: SpiConfig) -> Result<()> {
        self.config = config;
        self.configure().await
    }

    /// Set the SPI clock speed
    pub async fn set_speed(&mut self, speed: SpiSpeed) -> Result<()> {
        self.config.speed = speed;
        self.configure().await
    }

    /// Set which chip select to use
    pub async fn set_cs(&mut self, cs: ChipSelect) -> Result<()> {
        self.config.cs = cs;
        self.configure().await
    }

    /// Set the SPI mode
    pub async fn set_mode(&mut self, mode: SpiMode) -> Result<()> {
        self.config.mode = mode;
        self.configure().await
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl Ch347 {
    /// Request a CH347 device via the WebUSB permission prompt
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    /// It shows the browser's device picker filtered to CH347 devices (both T and F variants).
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        log::info!("Requesting CH347 device via WebUSB picker...");

        let device_info = nusb::request_device(USB_SELECTORS)
            .await
            .map_err(|e| Ch347Error::OpenFailed(format!("WebUSB request failed: {e}")))?
            .ok_or(Ch347Error::DeviceNotFound)?;

        log::info!(
            "CH347 device selected: VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        Ok(device_info)
    }

    /// Shutdown: clean up (WASM equivalent of Drop)
    pub async fn shutdown(&mut self) {
        // Drain any pending transfers
        #[cfg(not(target_arch = "wasm32"))]
        self.out_ep.cancel_all();
        while self.out_ep.pending() > 0 {
            let _ = ep_wait!(self.out_ep, Duration::from_secs(1));
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.in_ep.cancel_all();
        while self.in_ep.pending() > 0 {
            let _ = ep_wait!(self.in_ep, Duration::from_secs(1));
        }
        log::info!("CH347 shutdown complete");
    }
}

// ---------------------------------------------------------------------------
// Helper: find vendor-specific interface
// ---------------------------------------------------------------------------

/// Find the vendor-specific (class 0xFF) interface number for SPI.
/// CH347T uses interface 2, CH347F uses interface 4.
fn find_vendor_interface(config_desc: &nusb::descriptors::ConfigurationDescriptor) -> Result<u8> {
    for iface in config_desc.interface_alt_settings() {
        if iface.class() == 0xFF {
            // LIBUSB_CLASS_VENDOR_SPEC
            return Ok(iface.interface_number());
        }
    }
    Err(Ch347Error::OpenFailed(
        "Could not find vendor-specific interface".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Shared methods (async on every target)
// ---------------------------------------------------------------------------

impl Ch347 {
    /// Get the current SPI configuration
    pub fn config(&self) -> &SpiConfig {
        &self.config
    }

    /// Get the device variant
    pub fn variant(&self) -> Ch347Variant {
        self.variant
    }

    /// Configure the CH347 for SPI mode
    async fn configure(&mut self) -> Result<()> {
        let config_buf = self.config.build_config_buffer();

        // Send configuration
        self.usb_write(&config_buf).await?;

        // Read response (the device echoes back the config)
        let mut response = vec![0u8; 29];
        self.usb_read(&mut response).await?;

        log::info!(
            "CH347 configured: speed={}kHz, mode={}, cs={}",
            self.config.speed.to_khz(),
            self.config.mode as u8,
            self.config.cs as u8
        );

        Ok(())
    }

    /// Control chip select lines
    async fn cs_control(&mut self, assert: bool) -> Result<()> {
        let cs_value = if assert {
            CH347_CS_ASSERT | CH347_CS_CHANGE
        } else {
            CH347_CS_DEASSERT | CH347_CS_CHANGE
        };

        // Build CS control command
        // Format: [cmd, len_lo, len_hi, cs1_ctrl, 0, 0, 0, 0, cs2_ctrl, 0, 0, 0, 0]
        let mut cmd = [0u8; 13];
        cmd[0] = CH347_CMD_SPI_CS_CTRL;
        cmd[1] = 10; // payload length (low byte)
        cmd[2] = 0; // payload length (high byte)

        match self.config.cs {
            ChipSelect::CS0 => {
                cmd[3] = cs_value;
                cmd[8] = CH347_CS_IGNORE;
            }
            ChipSelect::CS1 => {
                cmd[3] = CH347_CS_IGNORE;
                cmd[8] = cs_value;
            }
        }

        self.usb_write(&cmd).await?;

        Ok(())
    }

    /// Write data via SPI (CS must already be asserted)
    async fn spi_write(&mut self, data: &[u8]) -> Result<()> {
        let mut bytes_written = 0;
        let mut resp_buf = [0u8; 4];

        while bytes_written < data.len() {
            let chunk_len = std::cmp::min(CH347_MAX_DATA_LEN, data.len() - bytes_written);
            let packet_len = chunk_len + 3;

            let mut buffer = vec![0u8; packet_len];
            buffer[0] = CH347_CMD_SPI_OUT;
            buffer[1] = (chunk_len & 0xFF) as u8;
            buffer[2] = ((chunk_len >> 8) & 0xFF) as u8;
            buffer[3..3 + chunk_len]
                .copy_from_slice(&data[bytes_written..bytes_written + chunk_len]);

            self.usb_write(&buffer).await?;

            // Read acknowledgment
            self.usb_read(&mut resp_buf).await?;

            bytes_written += chunk_len;
        }

        Ok(())
    }

    /// Read data via SPI (CS must already be asserted)
    async fn spi_read(&mut self, data: &mut [u8]) -> Result<()> {
        let readcnt = data.len();

        // Build read command
        // Format: [cmd, len_lo, len_hi, count_b0, count_b1, count_b2, count_b3]
        let command_buf = [
            CH347_CMD_SPI_IN,
            4,
            0,
            (readcnt & 0xFF) as u8,
            ((readcnt >> 8) & 0xFF) as u8,
            ((readcnt >> 16) & 0xFF) as u8,
            ((readcnt >> 24) & 0xFF) as u8,
        ];

        self.usb_write(&command_buf).await?;

        // Read response packets
        let mut bytes_read = 0;
        let mut buffer = vec![0u8; CH347_PACKET_SIZE];

        while bytes_read < readcnt {
            let received = self.usb_read(&mut buffer).await?;

            let payload = parse_spi_read_packet(&buffer[..received], readcnt - bytes_read)?;
            data[bytes_read..bytes_read + payload.len()].copy_from_slice(payload);
            bytes_read += payload.len();
        }

        Ok(())
    }

    /// Perform an SPI transfer (write then read)
    async fn spi_transfer(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        spi_transfer_with_io(self, write_data, read_buf).await
    }

    /// Write data to USB endpoint
    async fn usb_write(&mut self, data: &[u8]) -> Result<()> {
        let mut buf = Buffer::new(data.len());
        buf.extend_from_slice(data);

        self.out_ep.submit(buf);

        let completion = ep_wait!(self.out_ep, Duration::from_secs(5))
            .ok_or_else(|| Ch347Error::TransferFailed("USB write timed out".into()))?;

        completion
            .status
            .map_err(|e| Ch347Error::TransferFailed(e.to_string()))?;

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }

    /// Read data from USB endpoint
    async fn usb_read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let max_packet_size = self.in_ep.max_packet_size();
        // Request length must be multiple of max packet size
        let request_len = buffer.len().div_ceil(max_packet_size) * max_packet_size;
        let mut in_buf = Buffer::new(request_len);
        in_buf.set_requested_len(request_len);

        self.in_ep.submit(in_buf);

        let completion = ep_wait!(self.in_ep, Duration::from_secs(5))
            .ok_or_else(|| Ch347Error::TransferFailed("USB read timed out".into()))?;

        completion
            .status
            .map_err(|e| Ch347Error::TransferFailed(e.to_string()))?;

        let received = std::cmp::min(completion.actual_len, buffer.len());
        buffer[..received].copy_from_slice(&completion.buffer[..received]);

        log::trace!("USB read {} bytes", received);
        Ok(received)
    }
}

// ---------------------------------------------------------------------------
// SpiMaster trait implementation
// ---------------------------------------------------------------------------

impl SpiMaster for Ch347 {
    fn features(&self) -> SpiFeatures {
        // CH347 supports 4-byte addressing (software handled)
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // CH347 can handle large transfers
        // The protocol supports reading up to 2^32-1 bytes in one go
        64 * 1024
    }

    fn max_write_len(&self) -> usize {
        // CH347 can handle large transfers
        64 * 1024
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
        self.spi_transfer(&write_data, cmd.read_buf)
            .await
            .map_err(|_e| CoreError::ProgrammerError)?;

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        // Simple delay
        // The CH347 doesn't have a built-in delay command like the CH341A
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
// Option parsing (native only)
// ---------------------------------------------------------------------------

/// Parse programmer options for CH347
///
/// Supported options:
/// - `spispeed=<khz>`: SPI clock speed in kHz (default: 7500)
/// - `spimode=<0-3>`: SPI mode (default: 0)
/// - `cs=<0|1>`: Which chip select to use (default: 0)
///
/// # Example
///
/// ```ignore
/// let options = [("spispeed", "30000"), ("cs", "1")];
/// let config = parse_options(&options)?;
/// ```
#[cfg(feature = "std")]
pub fn parse_options(options: &[(&str, &str)]) -> Result<SpiConfig> {
    let mut config = SpiConfig::default();

    for (key, value) in options {
        match *key {
            "spispeed" => {
                let khz = crate::catalog::parse_speed_khz(value).ok_or_else(|| {
                    Ch347Error::ConfigError(format!("Invalid spispeed value: {value}"))
                })?;
                config.speed = SpiSpeed::from_khz(khz);
                log::debug!(
                    "Setting SPI speed to {}kHz (actual: {}kHz)",
                    khz,
                    config.speed.to_khz()
                );
            }
            "spimode" => {
                let mode: u8 = value.parse().map_err(|_| {
                    Ch347Error::ConfigError(format!("Invalid spimode value: {}", value))
                })?;
                config.mode = match mode {
                    0 => SpiMode::Mode0,
                    1 => SpiMode::Mode1,
                    2 => SpiMode::Mode2,
                    3 => SpiMode::Mode3,
                    _ => {
                        return Err(Ch347Error::ConfigError(format!(
                            "Invalid spimode: {} (must be 0-3)",
                            mode
                        )));
                    }
                };
            }
            "cs" => {
                let cs: u8 = value
                    .parse()
                    .map_err(|_| Ch347Error::ConfigError(format!("Invalid cs value: {}", value)))?;
                config.cs = match cs {
                    0 => ChipSelect::CS0,
                    1 => ChipSelect::CS1,
                    _ => {
                        return Err(Ch347Error::ConfigError(format!(
                            "Invalid cs: {} (must be 0 or 1)",
                            cs
                        )));
                    }
                };
            }
            _ => {
                return Err(Ch347Error::ConfigError(format!("unknown option: {key}")));
            }
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeSpiIo {
        events: Vec<&'static str>,
        fail: Option<&'static str>,
        fail_deassert: bool,
    }

    impl SpiIo for FakeSpiIo {
        async fn set_cs(&mut self, assert: bool) -> Result<()> {
            let event = if assert { "assert" } else { "deassert" };
            self.events.push(event);
            if self.fail == Some(event) || (!assert && self.fail_deassert) {
                return Err(Ch347Error::TransferFailed(event.into()));
            }
            Ok(())
        }

        async fn write(&mut self, _data: &[u8]) -> Result<()> {
            self.events.push("write");
            if self.fail == Some("write") {
                return Err(Ch347Error::TransferFailed("write".into()));
            }
            Ok(())
        }

        async fn read(&mut self, _data: &mut [u8]) -> Result<()> {
            self.events.push("read");
            if self.fail == Some("read") {
                return Err(Ch347Error::TransferFailed("read".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn spi_transfer_deasserts_after_each_phase_error() {
        for (failure, expected) in [
            ("assert", vec!["assert", "deassert"]),
            ("write", vec!["assert", "write", "deassert"]),
            ("read", vec!["assert", "write", "read", "deassert"]),
            ("deassert", vec!["assert", "write", "read", "deassert"]),
        ] {
            let mut io = FakeSpiIo {
                fail: Some(failure),
                fail_deassert: true,
                ..Default::default()
            };
            let error =
                futures_lite::future::block_on(spi_transfer_with_io(&mut io, &[1], &mut [0]))
                    .unwrap_err();
            assert!(matches!(error, Ch347Error::TransferFailed(message) if message == failure));
            assert_eq!(io.events, expected);
        }
    }

    #[test]
    fn spi_read_packet_rejects_empty_and_malformed_responses() {
        for packet in [
            &[][..],
            &[CH347_CMD_SPI_IN, 0, 0],
            &[CH347_CMD_SPI_IN, 2, 0, 7],
            &[CH347_CMD_SPI_OUT, 1, 0, 7],
        ] {
            assert!(matches!(
                parse_spi_read_packet(packet, 3),
                Err(Ch347Error::InvalidResponse(_))
            ));
        }
        assert_eq!(
            parse_spi_read_packet(&[CH347_CMD_SPI_IN, 2, 0, 4, 5], 1).unwrap(),
            &[4]
        );
    }
}
