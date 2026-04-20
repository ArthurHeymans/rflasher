//! FT4222H device implementation
//!
//! This module provides the main `Ft4222` struct that implements USB
//! communication with the FT4222H SPI master and the `SpiMaster` trait.
//!
//! The FT4222H uses a vendor-specific USB protocol (not MPSSE/libftdi).
//! This implementation is based on flashprog's ft4222_spi.c which uses
//! raw libusb without the proprietary LibFT4222.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, ControlIn, ControlOut, ControlType, In, Out, Recipient};
use nusb::{Endpoint, Interface, MaybeFuture};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::default_execute_with_vec;
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{IoMode as CoreIoMode, SpiCommand};

use crate::error::{Ft4222Error, Result};
use crate::protocol::*;

/// FT4222H USB SPI Master programmer
///
/// This struct represents a connection to an FT4222H USB device and implements
/// the `SpiMaster` trait for communicating with SPI flash chips.
///
/// The FT4222H supports:
/// - Single, Dual, and Quad SPI modes
/// - SPI speeds from ~47 kHz to 40 MHz
/// - Up to 4 chip select lines (depending on mode)
/// - 4-byte addressing (software handled)
///
/// # Features
///
/// - High-speed USB 2.0 (480 Mbps)
/// - Configurable SPI clock from system clocks (60/24/48/80 MHz) with divisors
/// - Multiple I/O modes: single (1-1-1), dual (1-1-2, 1-2-2), quad (1-1-4, 1-4-4, 4-4-4)
/// - Pure USB implementation (no vendor library required)
pub struct Ft4222 {
    /// USB interface
    interface: Interface,
    /// Current SPI configuration
    config: SpiConfig,
    /// Selected clock configuration
    clock_config: ClockConfig,
    /// Control interface index (from USB descriptor)
    control_index: u8,
    /// Bulk IN endpoint address
    in_ep: u8,
    /// Bulk OUT endpoint address
    out_ep: u8,
    /// Current I/O lines mode
    io_lines: u8,
    /// Cached bulk OUT endpoint. Held for the lifetime of the device so each SPI
    /// transfer doesn't pay the cost of descriptor lookup, Arc allocation, and
    /// the interface-level mutex that `Interface::endpoint` acquires.
    out_endpoint: Option<Endpoint<Bulk, Out>>,
    /// Cached bulk IN endpoint (see `out_endpoint`).
    in_endpoint: Option<Endpoint<Bulk, In>>,
    /// Cached `max_packet_size` for the bulk IN endpoint.
    in_max_packet_size: usize,
}

impl Ft4222 {
    /// Open an FT4222H device with default configuration
    ///
    /// Searches for an FT4222H device (VID:0403 PID:601c) and opens it.
    /// Returns an error if no device is found or if the device cannot be opened.
    pub fn open() -> Result<Self> {
        Self::open_with_config(SpiConfig::default())
    }

    /// Open an FT4222H device with custom configuration
    pub fn open_with_config(config: SpiConfig) -> Result<Self> {
        Self::open_nth_with_config(0, config)
    }

    /// Open the nth FT4222H device (0-indexed) with default configuration
    ///
    /// Useful when multiple FT4222H devices are connected.
    pub fn open_nth(index: usize) -> Result<Self> {
        Self::open_nth_with_config(index, SpiConfig::default())
    }

    /// Open the nth FT4222H device with custom configuration
    pub fn open_nth_with_config(index: usize, config: SpiConfig) -> Result<Self> {
        // Find FT4222H devices
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == FTDI_VID && d.product_id() == FT4222H_PID)
            .collect();

        let device_info = devices.get(index).ok_or(Ft4222Error::DeviceNotFound)?;

        log::info!(
            "Opening FT4222H device at bus {} address {}",
            device_info.busnum(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .wait()
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?;

        log::debug!(
            "Device: VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        // Get configuration descriptor to find endpoints
        let config_desc = device
            .active_configuration()
            .map_err(|e| Ft4222Error::OpenFailed(format!("Failed to get config: {}", e)))?;

        // Find the vendor-specific interface for SPI
        let mut spi_interface: Option<u8> = None;
        let mut in_ep: Option<u8> = None;
        let mut out_ep: Option<u8> = None;

        for iface in config_desc.interface_alt_settings() {
            // Look for vendor-specific class (0xFF) or interface 0
            if iface.class() == 0xFF || iface.interface_number() == 0 {
                for ep in iface.endpoints() {
                    if ep.transfer_type() == nusb::descriptors::TransferType::Bulk {
                        if ep.direction() == nusb::transfer::Direction::In {
                            in_ep = Some(ep.address());
                        } else {
                            out_ep = Some(ep.address());
                        }
                    }
                }
                if in_ep.is_some() && out_ep.is_some() {
                    spi_interface = Some(iface.interface_number());
                    break;
                }
            }
        }

        let iface_num = spi_interface.ok_or_else(|| {
            Ft4222Error::OpenFailed("Could not find suitable USB interface".to_string())
        })?;
        let in_ep = in_ep
            .ok_or_else(|| Ft4222Error::OpenFailed("Could not find IN endpoint".to_string()))?;
        let out_ep = out_ep
            .ok_or_else(|| Ft4222Error::OpenFailed("Could not find OUT endpoint".to_string()))?;

        log::debug!(
            "Using interface {}, IN EP 0x{:02X}, OUT EP 0x{:02X}",
            iface_num,
            in_ep,
            out_ep
        );

        // Claim interface
        let interface = device
            .claim_interface(iface_num)
            .wait()
            .map_err(|e| Ft4222Error::ClaimFailed(e.to_string()))?;

        // Calculate clock configuration
        let clock_config = find_clock_config(config.speed_khz);

        // Determine control index based on number of interfaces (matching flashprog)
        // LibFT4222 sets control_index = 1 if there are multiple interfaces, 0 otherwise
        let num_interfaces = config_desc.num_interfaces();
        let control_index = if num_interfaces > 1 { 1 } else { 0 };

        log::debug!(
            "Number of interfaces: {}, control_index: {}",
            num_interfaces,
            control_index
        );

        let mut ft4222 = Self {
            interface,
            config,
            clock_config,
            control_index,
            in_ep,
            out_ep,
            io_lines: 1, // Start with single I/O
            out_endpoint: None,
            in_endpoint: None,
            in_max_packet_size: 0,
        };

        // Claim the bulk endpoints up-front. `Interface::endpoint` allocates and
        // takes an internal mutex on every call, so we want to hit it exactly
        // once per device lifetime rather than once per SPI transfer.
        let out_endpoint: Endpoint<Bulk, Out> = ft4222
            .interface
            .endpoint(ft4222.out_ep)
            .map_err(|e| Ft4222Error::OpenFailed(format!("Failed to claim OUT endpoint: {e}")))?;
        let in_endpoint: Endpoint<Bulk, In> = ft4222
            .interface
            .endpoint(ft4222.in_ep)
            .map_err(|e| Ft4222Error::OpenFailed(format!("Failed to claim IN endpoint: {e}")))?;
        ft4222.in_max_packet_size = in_endpoint.max_packet_size();
        ft4222.out_endpoint = Some(out_endpoint);
        ft4222.in_endpoint = Some(in_endpoint);

        // Initialize the device
        ft4222.init()?;

        Ok(ft4222)
    }

    /// Initialize the FT4222H for SPI master mode
    fn init(&mut self) -> Result<()> {
        // Query version info
        let (chip_version, version2, version3) = self.get_version()?;
        log::info!(
            "FT4222H version: chip=0x{:08X} (0x{:08X} 0x{:08X})",
            chip_version,
            version2,
            version3
        );

        // Query number of channels
        let channels = self.get_num_channels()?;
        log::debug!("FT4222H channels: {}", channels);

        // Validate CS selection
        if self.config.cs >= channels {
            return Err(Ft4222Error::InvalidParameter(format!(
                "CS{} not available (device has {} channels)",
                self.config.cs, channels
            )));
        }

        // Reset the device
        self.reset()?;

        // Set system clock
        self.set_sys_clock(self.clock_config.sys_clock)?;

        // Configure SPI master mode
        self.configure_spi_master()?;

        log::info!(
            "FT4222H configured: SPI clock = {} kHz, CS = {}, I/O mode = {:?}",
            self.clock_config.spi_clock_khz(),
            self.config.cs,
            self.config.io_mode
        );

        Ok(())
    }

    /// Get device version information (matching flashprog's ft4222_get_version)
    ///
    /// Returns (chip_version, version2, version3) - flashprog reads 12 bytes
    fn get_version(&self) -> Result<(u32, u32, u32)> {
        let data = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: FT4222_INFO_REQUEST,
                    value: FT4222_GET_VERSION,
                    index: self.control_index as u16,
                    length: 12, // flashprog requests 12 bytes
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Failed to get version: {}", e)))?;

        if data.len() < 12 {
            return Err(Ft4222Error::InvalidResponse(format!(
                "Version response too short: {} < 12",
                data.len()
            )));
        }

        // flashprog reads these as big-endian
        let chip_version = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let version2 = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let version3 = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        Ok((chip_version, version2, version3))
    }

    /// Get number of CS channels available (matching flashprog's ft4222_get_num_channels)
    ///
    /// This queries GET_CONFIG and parses the mode byte to determine channel count.
    fn get_num_channels(&self) -> Result<u8> {
        let data = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: FT4222_INFO_REQUEST,
                    value: FT4222_GET_CONFIG,
                    index: self.control_index as u16,
                    length: 13,
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Failed to get config: {}", e)))?;

        if data.is_empty() {
            return Err(Ft4222Error::InvalidResponse(
                "Empty response for config".into(),
            ));
        }

        // Parse mode byte to determine number of channels (matching flashprog)
        let channels = match data[0] {
            0 => 1, // Mode 0
            1 => 3, // Mode 1
            2 => 4, // Mode 2
            3 => 1, // Mode 3
            mode => {
                return Err(Ft4222Error::InvalidResponse(format!(
                    "Unknown mode byte: 0x{:02x}",
                    mode
                )))
            }
        };

        log::debug!("FT4222H mode: {}, channels: {}", data[0], channels);
        Ok(channels)
    }

    /// Reset the device (matching flashprog's ft4222_reset)
    fn reset(&self) -> Result<()> {
        // Reset SIO - note: wIndex = 0, not control_index
        self.control_out_with_index(FT4222_RESET_REQUEST, FT4222_RESET_SIO, 0, &[])?;

        // Flush output buffer multiple times (flashprog does this 6 times)
        self.flush()?;

        log::debug!("FT4222H reset complete");
        Ok(())
    }

    /// Flush buffers (matching flashprog's ft4222_flush)
    fn flush(&self) -> Result<()> {
        // Flush output buffer 6 times (as flashprog does)
        for _ in 0..6 {
            if let Err(e) = self.control_out(FT4222_RESET_REQUEST, FT4222_OUTPUT_FLUSH, &[]) {
                log::warn!("FT4222 output flush failed: {}", e);
                break;
            }
        }

        // Flush input buffer once
        if let Err(e) = self.control_out(FT4222_RESET_REQUEST, FT4222_INPUT_FLUSH, &[]) {
            log::warn!("FT4222 input flush failed: {}", e);
        }

        Ok(())
    }

    /// Set system clock
    fn set_sys_clock(&self, clock: SystemClock) -> Result<()> {
        self.config_request(FT4222_SET_CLOCK, clock.index() as u8)?;
        log::debug!("Set system clock to {} MHz", clock.to_khz() / 1000);
        Ok(())
    }

    /// Configure SPI master mode (matching flashprog's ft4222_configure_spi_master)
    fn configure_spi_master(&mut self) -> Result<()> {
        let cs = self.config.cs;

        // Reset transaction for this CS (idx => cs)
        self.config_request(FT4222_SPI_RESET_TRANSACTION, cs)?;

        // Set I/O lines (start with single)
        self.io_lines = 1;
        self.config_request(FT4222_SPI_SET_IO_LINES, 1)?;

        // Set clock divisor
        self.config_request(
            FT4222_SPI_SET_CLK_DIV,
            self.clock_config.divisor.value() as u8,
        )?;

        // Set clock polarity (idle low for SPI mode 0)
        self.config_request(FT4222_SPI_SET_CLK_IDLE, FT4222_CLK_IDLE_LOW)?;

        // Set clock phase (capture on leading edge for SPI mode 0)
        self.config_request(FT4222_SPI_SET_CAPTURE, FT4222_CLK_CAPTURE_LEADING)?;

        // Set CS polarity (active low)
        self.config_request(FT4222_SPI_SET_CS_ACTIVE, FT4222_CS_ACTIVE_LOW)?;

        // Set CS mask for selected chip select
        self.config_request(FT4222_SPI_SET_CS_MASK, 1 << cs)?;

        // Set mode to SPI Master
        self.config_request(FT4222_SET_MODE, FT4222_MODE_SPI_MASTER)?;

        Ok(())
    }

    /// Set I/O lines mode (matching flashprog's ft4222_spi_set_io_lines)
    fn set_io_lines(&mut self, lines: u8) -> Result<()> {
        if lines != self.io_lines {
            self.config_request(FT4222_SPI_SET_IO_LINES, lines)?;
            // Reset line number after changing I/O lines
            self.config_request(FT4222_SPI_RESET, FT4222_SPI_RESET_LINE_NUM)?;
            self.io_lines = lines;
            log::trace!("Set I/O lines to {}", lines);
        }
        Ok(())
    }

    /// Send a control OUT transfer with default control_index
    fn control_out(&self, request: u8, value: u16, data: &[u8]) -> Result<()> {
        self.control_out_with_index(request, value, self.control_index as u16, data)
    }

    /// Send a control OUT transfer with explicit index
    fn control_out_with_index(
        &self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    data,
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Control transfer failed: {}", e)))?;

        Ok(())
    }

    /// Send a config request (matching flashprog's ft4222_config_request)
    ///
    /// The FT4222H encodes config commands with:
    /// - wValue = (data << 8) | cmd
    /// - wIndex = control_index
    fn config_request(&self, cmd: u8, data: u8) -> Result<()> {
        let value = ((data as u16) << 8) | (cmd as u16);
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: FT4222_CONFIG_REQUEST,
                    value,
                    index: self.control_index as u16,
                    data: &[],
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Control transfer failed: {}", e)))?;

        Ok(())
    }

    /// Perform a single-I/O SPI transfer (full duplex) with pipelined USB transfers
    ///
    /// This is used for standard 1-1-1 mode transfers. Uses pipelining to submit
    /// all USB transfers before waiting, which prevents RX buffer overflow and
    /// maximizes throughput.
    fn spi_transfer_single(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        // Set to single I/O mode
        self.set_io_lines(1)?;

        // Total number of real SPI bytes we expect to clock (write phase + read phase).
        // In single-I/O mode the FT4222H is full-duplex: it clocks out whatever we send
        // and clocks in continuously. We'll discard the first `write_data.len()` bytes
        // (collected while our command was being sent) and keep the remaining `read_len`.
        let total_len = write_data.len() + read_len;

        if total_len == 0 {
            return Ok(Vec::new());
        }

        let max_packet_size = self.in_max_packet_size;
        // Borrow the cached endpoints. These are claimed once at open() time rather
        // than re-acquired per call: `Interface::endpoint` allocates a fresh Arc +
        // VecDeque + Notify and takes an internal mutex on every invocation, which
        // is disastrous when called millions of times (e.g. 262144 AAI words).
        let out_ep = self
            .out_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("OUT endpoint missing".into()))?;
        let in_ep = self
            .in_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("IN endpoint missing".into()))?;

        // === OUTBOUND PHASE ===
        // Mirror flashprog's ft4222_spi_send_command: send three OUT transfers:
        //   1. `write_data`                — real command bytes
        //   2. `read_len` dummy bytes      — to clock out the response
        //   3. empty packet                — to deassert CS
        // These are submitted back-to-back so the FT4222H executes them as one
        // continuous SPI transaction.

        let mut write_buf = Buffer::new(write_data.len());
        write_buf.extend_from_slice(write_data);
        out_ep.submit(write_buf);

        if read_len > 0 {
            let mut dummy_buf = Buffer::new(read_len);
            dummy_buf.extend_fill(read_len, 0xff);
            out_ep.submit(dummy_buf);
        }

        out_ep.submit(Buffer::new(0)); // CS deassert

        // === INBOUND PHASE ===
        // Following flashprog's ft4222_async_read loop: keep submitting IN transfers
        // and accumulating real payload bytes until we have `total_len`. Each USB IN
        // packet is 512 bytes long and starts with a 2-byte modem-status header. When
        // the FT4222H is idle (e.g. between wait_ready polls) it emits periodic 2-byte
        // modem-status-only packets that must be discarded. A bulk transfer terminates
        // early on a short packet, so these stale packets can truncate a larger URB;
        // re-submitting until we have all the real bytes handles that cleanly.
        let mut raw = Vec::<u8>::with_capacity(total_len);
        let mut real_bytes = 0usize;

        while real_bytes < total_len {
            let remaining = total_len - real_bytes;
            // Request size = enough USB packets to cover the remaining real bytes,
            // bounded by READ_BUFFER_SIZE (matches flashprog).
            let bytes_per_packet = max_packet_size - MODEM_STATUS_SIZE;
            let packets_needed = remaining.div_ceil(bytes_per_packet);
            let request_len =
                (packets_needed * max_packet_size).min(crate::protocol::READ_BUFFER_SIZE);

            let mut in_buf = Buffer::new(request_len);
            in_buf.set_requested_len(request_len);
            in_ep.submit(in_buf);

            let completion = in_ep
                .wait_next_complete(Duration::from_secs(30))
                .ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk read failed: {e}")))?;

            let data = &completion.buffer[..completion.actual_len];
            for packet in data.chunks(max_packet_size) {
                if packet.len() <= MODEM_STATUS_SIZE {
                    // Modem-status-only packet: no payload, just drop it.
                    continue;
                }
                let payload = &packet[MODEM_STATUS_SIZE..];
                let to_copy = payload.len().min(total_len - real_bytes);
                raw.extend_from_slice(&payload[..to_copy]);
                real_bytes += to_copy;
                if real_bytes >= total_len {
                    break;
                }
            }
        }

        // === WAIT FOR OUTBOUND COMPLETIONS ===
        // Drain OUT completions. All transfers are expected to succeed.
        let expected_out = if read_len > 0 { 3 } else { 2 };
        for _ in 0..expected_out {
            let completion = out_ep
                .wait_next_complete(Duration::from_secs(30))
                .ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk write failed: {e}")))?;
        }

        log::trace!(
            "SPI transfer: wrote {} bytes, read {} bytes (got {} payload bytes)",
            write_data.len(),
            read_len,
            raw.len()
        );
        let result = raw;

        // Skip bytes received during write phase, return only read data
        if result.len() >= total_len {
            Ok(result[write_data.len()..].to_vec())
        } else {
            Err(Ft4222Error::InvalidResponse(format!(
                "Expected {} bytes, got {}",
                total_len,
                result.len()
            )))
        }
    }

    /// Execute a `SpiCommand` in dual/quad/QPI mode.
    ///
    /// Mirrors flashprog's `ft4222_spi_send_multi_io`. The key points:
    ///
    /// - The dummy phase is represented as `high_z_bytes` skipped from the
    ///   READ buffer, NOT as data in the write phase. This is because after
    ///   the mode byte / opcode, the chip owns the I/O lines — any extra
    ///   write bytes would collide with the chip's output.
    /// - The mode byte (when applicable for 1-2-2 / 1-4-4 / 4-4-4) is emitted
    ///   as one write byte at the multi-IO rate.
    /// - `cmd.dummy_cycles` is interpreted as total clock cycles at the
    ///   I/O mode's lane count between end-of-address and start-of-data
    ///   (i.e. it includes the mode byte time when one is sent).
    fn execute_multi_io(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // io_width is the number of lanes during the multi phase.
        let io_width: usize = match cmd.io_mode {
            CoreIoMode::Single => 1,
            CoreIoMode::DualOut | CoreIoMode::DualIo => 2,
            CoreIoMode::QuadOut | CoreIoMode::QuadIo | CoreIoMode::Qpi => 4,
        };
        let io_lines = io_width as u8;

        // Opcode and address are built explicitly — we do not use
        // cmd.encode_header because it adds dummy bytes into the write phase,
        // which is wrong for multi-IO (they must go on the read side as
        // high-Z to be skipped).
        let opcode = [cmd.opcode];
        let addr_width = cmd.address_width.bytes() as usize;
        let mut addr_bytes = [0u8; 4];
        if let Some(addr) = cmd.address {
            cmd.address_width.encode(addr, &mut addr_bytes);
        }
        let addr_slice = &addr_bytes[..addr_width];

        // Compute dummy time at the multi-IO rate. dummy_cycles is the total
        // lane-clocks of non-data time between address end and data start
        // (includes mode byte, if any).
        let dummy_bytes_total = (cmd.dummy_cycles as usize * io_width).div_ceil(8);

        // Which modes use a mode byte (M7-M0). DIO/QIO/QPI do; DOUT/QOUT
        // do not. For the ones that do, we use 0xFF — its top nibble
        // (0xF) is NOT 0xA, so continuous-read mode is not enabled on
        // Winbond-family chips.
        let uses_mode_byte = matches!(
            cmd.io_mode,
            CoreIoMode::DualIo | CoreIoMode::QuadIo | CoreIoMode::Qpi
        );
        let mode_byte_len = if uses_mode_byte && dummy_bytes_total > 0 {
            1
        } else {
            0
        };
        let mode_byte = [0xFFu8];

        // The remaining dummy bytes go on the read side and get skipped.
        let high_z_bytes = dummy_bytes_total.saturating_sub(mode_byte_len);

        // Split into single-IO and multi-IO write phases per io_mode:
        //   1-1-x: opcode + addr + write_data -> single;    multi empty.
        //   1-x-x: opcode -> single;   addr + M + write_data -> multi.
        //   4-4-4: empty single;       opcode + addr + M + write_data -> multi.
        let (single_slice, multi_slice): (Vec<u8>, Vec<u8>) = match cmd.io_mode {
            CoreIoMode::Single => (opcode.to_vec(), Vec::new()),
            CoreIoMode::DualOut | CoreIoMode::QuadOut => {
                // 1-1-x: everything in the write phase is single-wire.
                let mut s = Vec::with_capacity(1 + addr_width + cmd.write_data.len());
                s.extend_from_slice(&opcode);
                s.extend_from_slice(addr_slice);
                s.extend_from_slice(cmd.write_data);
                (s, Vec::new())
            }
            CoreIoMode::DualIo | CoreIoMode::QuadIo => {
                // 1-x-x: opcode single, rest multi.
                let s = opcode.to_vec();
                let mut m = Vec::with_capacity(addr_width + mode_byte_len + cmd.write_data.len());
                m.extend_from_slice(addr_slice);
                if mode_byte_len > 0 {
                    m.extend_from_slice(&mode_byte);
                }
                m.extend_from_slice(cmd.write_data);
                (s, m)
            }
            CoreIoMode::Qpi => {
                // 4-4-4: all multi.
                let mut m =
                    Vec::with_capacity(1 + addr_width + mode_byte_len + cmd.write_data.len());
                m.extend_from_slice(&opcode);
                m.extend_from_slice(addr_slice);
                if mode_byte_len > 0 {
                    m.extend_from_slice(&mode_byte);
                }
                m.extend_from_slice(cmd.write_data);
                (Vec::new(), m)
            }
        };

        log::trace!(
            "ft4222 multi-io: opcode=0x{:02X} io_lines={} single={}B multi={}B high_z={}B read={}B",
            cmd.opcode,
            io_lines,
            single_slice.len(),
            multi_slice.len(),
            high_z_bytes,
            cmd.read_buf.len(),
        );

        self.spi_transfer_multi_into(
            &single_slice,
            &multi_slice,
            cmd.read_buf,
            high_z_bytes,
            io_lines,
        )
        .map_err(|e| {
            log::error!("ft4222 multi-io transfer failed: {e}");
            CoreError::ProgrammerError
        })?;
        Ok(())
    }

    /// Perform a multi-I/O SPI transfer (half duplex) writing the read data
    /// directly into `read_buf` with `high_z_bytes` of dummy skip.
    ///
    /// Format on the wire:
    /// `| header(5B) | single_data | multi_write_data |` (bulk OUT),
    /// then `high_z_bytes` bytes + `read_buf.len()` bytes on bulk IN, with
    /// the first `high_z_bytes` discarded.
    ///
    /// Mirrors flashprog's `ft4222_spi_send_multi_io`.
    fn spi_transfer_multi_into(
        &mut self,
        single_data: &[u8],
        multi_write_data: &[u8],
        read_buf: &mut [u8],
        high_z_bytes: usize,
        io_lines: u8,
    ) -> Result<()> {
        let read_total = high_z_bytes + read_buf.len();

        // Validate lengths
        if single_data.len() > MULTI_IO_MAX_SINGLE {
            return Err(Ft4222Error::InvalidParameter(format!(
                "Single phase too long: {} > {}",
                single_data.len(),
                MULTI_IO_MAX_SINGLE
            )));
        }
        if multi_write_data.len() > MULTI_IO_MAX_DATA {
            return Err(Ft4222Error::InvalidParameter(format!(
                "Multi-write phase too long: {} > {}",
                multi_write_data.len(),
                MULTI_IO_MAX_DATA
            )));
        }
        if read_total > MULTI_IO_MAX_DATA {
            return Err(Ft4222Error::InvalidParameter(format!(
                "Multi-read total too long: {} > {}",
                read_total, MULTI_IO_MAX_DATA
            )));
        }

        // Set I/O lines for multi-I/O phase
        self.set_io_lines(io_lines)?;

        // Build multi-I/O header (big-endian lengths, matching flashprog's
        // ft4222_spi_send_multi_io):
        //   byte 0: 0x80 | single_len (4 bits)
        //   bytes 1-2: multi_write_len (big-endian u16)
        //   bytes 3-4: read_total (big-endian u16) — includes high_z skip
        let mut header = [0u8; MULTI_IO_HEADER_SIZE];
        header[0] = MULTI_IO_MAGIC | (single_data.len() as u8 & 0x0F);
        header[1] = ((multi_write_data.len() >> 8) & 0xFF) as u8;
        header[2] = (multi_write_data.len() & 0xFF) as u8;
        header[3] = ((read_total >> 8) & 0xFF) as u8;
        header[4] = (read_total & 0xFF) as u8;

        // Assemble the single-shot write: header + single phase + multi phase.
        let write_total = MULTI_IO_HEADER_SIZE + single_data.len() + multi_write_data.len();

        // Use the cached endpoints (claimed once at open()). Creating fresh
        // `Endpoint` instances via `self.interface.endpoint()` while the
        // cached ones are held fails with "endpoint already in use".
        let max_packet_size = self.in_max_packet_size;
        let out_ep = self
            .out_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("OUT endpoint missing".into()))?;

        let mut out_buf = Buffer::new(write_total);
        out_buf.extend_from_slice(&header);
        out_buf.extend_from_slice(single_data);
        out_buf.extend_from_slice(multi_write_data);
        out_ep.submit(out_buf);

        // If nothing to read, wait for the write completion and return.
        if read_total == 0 {
            let completion = out_ep
                .wait_next_complete(Duration::from_secs(30))
                .ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk write failed: {e}")))?;
            return Ok(());
        }

        // Pipeline: submit write, kick off reads, then drain both.
        let in_ep = self
            .in_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("IN endpoint missing".into()))?;

        // Collect into `raw` then split high_z vs payload.
        let mut raw = Vec::<u8>::with_capacity(read_total);
        let mut real_bytes = 0usize;

        while real_bytes < read_total {
            let remaining = read_total - real_bytes;
            let bytes_per_packet = max_packet_size - MODEM_STATUS_SIZE;
            let packets_needed = remaining.div_ceil(bytes_per_packet);
            let request_len = (packets_needed * max_packet_size).min(READ_BUFFER_SIZE);

            let mut in_buf = Buffer::new(request_len);
            in_buf.set_requested_len(request_len);
            in_ep.submit(in_buf);

            let completion = in_ep
                .wait_next_complete(Duration::from_secs(30))
                .ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk read failed: {e}")))?;

            let data = &completion.buffer[..completion.actual_len];
            for packet in data.chunks(max_packet_size) {
                if packet.len() <= MODEM_STATUS_SIZE {
                    continue;
                }
                let payload = &packet[MODEM_STATUS_SIZE..];
                let to_copy = payload.len().min(read_total - real_bytes);
                raw.extend_from_slice(&payload[..to_copy]);
                real_bytes += to_copy;
                if real_bytes >= read_total {
                    break;
                }
            }
        }

        // Drain the write completion.
        let out_ep = self
            .out_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("OUT endpoint missing".into()))?;
        let completion = out_ep
            .wait_next_complete(Duration::from_secs(30))
            .ok_or(Ft4222Error::Timeout)?;
        completion
            .status
            .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk write failed: {e}")))?;

        if raw.len() < read_total {
            return Err(Ft4222Error::InvalidResponse(format!(
                "short read: wanted {read_total}, got {}",
                raw.len()
            )));
        }
        // Skip high-Z bytes, copy actual data.
        read_buf.copy_from_slice(&raw[high_z_bytes..high_z_bytes + read_buf.len()]);
        Ok(())
    }

    /// List all connected FT4222H devices
    pub fn list_devices() -> Result<Vec<Ft4222DeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == FTDI_VID && d.product_id() == FT4222H_PID)
            .map(|d| Ft4222DeviceInfo {
                bus: d.busnum(),
                address: d.device_address(),
            })
            .collect();

        Ok(devices)
    }

    /// Get the current SPI configuration
    pub fn config(&self) -> &SpiConfig {
        &self.config
    }

    /// Get the actual SPI clock speed in kHz
    pub fn actual_speed_khz(&self) -> u32 {
        self.clock_config.spi_clock_khz()
    }
}

impl SpiMaster for Ft4222 {
    fn features(&self) -> SpiFeatures {
        // FT4222H always does 4-byte addressing in software. Multi-IO
        // capability depends on how the user configured the device:
        //   IoMode::Single -> only single
        //   IoMode::Dual   -> single + dual (both 1-1-2 and 1-2-2)
        //   IoMode::Quad   -> single + dual + quad + QPI (4-4-4)
        let mut f = SpiFeatures::FOUR_BYTE_ADDR;
        match self.config.io_mode {
            IoMode::Single => {}
            IoMode::Dual => {
                f |= SpiFeatures::DUAL_IN | SpiFeatures::DUAL_IO;
            }
            IoMode::Quad => {
                f |= SpiFeatures::DUAL_IN
                    | SpiFeatures::DUAL_IO
                    | SpiFeatures::QUAD_IN
                    | SpiFeatures::QUAD_IO
                    | SpiFeatures::QPI;
            }
        }
        f
    }

    fn max_read_len(&self) -> usize {
        // With pipelined transfers, we can use much larger chunks since
        // read and write happen concurrently, preventing RX buffer overflow.
        // flashprog uses 65530.
        65530
    }

    fn max_write_len(&self) -> usize {
        65530
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        match cmd.io_mode {
            CoreIoMode::Single => {
                default_execute_with_vec(cmd, self.features(), |write_data, read_len| {
                    self.spi_transfer_single(write_data, read_len)
                        .map_err(|_| CoreError::ProgrammerError)
                })
            }
            _ => self.execute_multi_io(cmd),
        }
    }

    fn delay_us(&mut self, us: u32) {
        if us == 0 {
            return;
        }
        // For short delays (<100 us), thread::sleep is unusable: Linux timer granularity
        // and scheduler wake-up latency push actual sleep time to ~50-100 us minimum.
        // That's catastrophic inside tight SPI polling loops (e.g. AAI word program,
        // which polls WIP every 10 us between 2-byte writes) — we'd lose tens of
        // seconds per flash MiB to scheduler jitter alone.
        //
        // Match flashprog's approach: busy-wait for sub-100 us delays, and fall
        // back to thread::sleep for longer ones where precision doesn't matter.
        const SPIN_THRESHOLD_US: u32 = 100;
        if us < SPIN_THRESHOLD_US {
            let deadline = std::time::Instant::now() + Duration::from_micros(us as u64);
            while std::time::Instant::now() < deadline {
                std::hint::spin_loop();
            }
        } else {
            std::thread::sleep(Duration::from_micros(us as u64));
        }
    }
}

/// Information about a connected FT4222H device
#[derive(Debug, Clone)]
pub struct Ft4222DeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
}

impl std::fmt::Display for Ft4222DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FT4222H at bus {} address {}", self.bus, self.address)
    }
}

/// Parse programmer options for FT4222
///
/// Supported options:
/// - `spispeed=<khz>`: Target SPI clock speed in kHz (default: 10000)
/// - `cs=<0-3>`: Which chip select to use (default: 0)
/// - `iomode=<single|dual|quad>`: I/O mode (default: single)
///
/// # Example
///
/// ```ignore
/// let options = [("spispeed", "30000"), ("cs", "1")];
/// let config = parse_options(&options)?;
/// ```
pub fn parse_options(options: &[(&str, &str)]) -> Result<SpiConfig> {
    let mut config = SpiConfig::default();

    for (key, value) in options {
        match *key {
            "spispeed" => {
                let khz: u32 = value.parse().map_err(|_| {
                    Ft4222Error::InvalidParameter(format!("Invalid spispeed value: {}", value))
                })?;
                config.speed_khz = khz;
                log::debug!("Setting target SPI speed to {} kHz", khz);
            }
            "cs" => {
                let cs: u8 = value.parse().map_err(|_| {
                    Ft4222Error::InvalidParameter(format!("Invalid cs value: {}", value))
                })?;
                if cs > 3 {
                    return Err(Ft4222Error::InvalidParameter(format!(
                        "Invalid cs: {} (must be 0-3)",
                        cs
                    )));
                }
                config.cs = cs;
            }
            "iomode" => {
                config.io_mode = IoMode::parse(value).ok_or_else(|| {
                    Ft4222Error::InvalidParameter(format!(
                        "Invalid iomode: {} (must be single, dual, or quad)",
                        value
                    ))
                })?;
            }
            _ => {
                log::warn!("Unknown FT4222 option: {}={}", key, value);
            }
        }
    }

    Ok(config)
}
