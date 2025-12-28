//! Host-side USB transport and SpiMaster implementation
//!
//! This module provides the `PostcardProgrammer` struct which implements
//! communication with a postcard-rpc based flash programmer over USB.

use postcard_rpc::header::VarSeqKind;
use postcard_rpc::host_client::HostClient;

use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::error::{Error, Result};
use crate::protocol::*;

/// Postcard-RPC based flash programmer
///
/// This struct represents a connection to a postcard-rpc based programmer
/// and implements the `SpiMaster` trait for communicating with SPI flash chips.
pub struct PostcardProgrammer {
    /// Postcard-RPC host client
    client: HostClient<ErrorResp>,
    /// Cached device info
    info: Option<DeviceInfo>,
    /// Tokio runtime handle for blocking operations
    runtime: tokio::runtime::Runtime,
}

impl PostcardProgrammer {
    /// Open a connection to a USB programmer by VID/PID
    ///
    /// # Arguments
    /// * `vid` - USB Vendor ID
    /// * `pid` - USB Product ID
    ///
    /// # Example
    /// ```ignore
    /// let programmer = PostcardProgrammer::open_usb(0x16c0, 0x27dd)?;
    /// ```
    pub fn open_usb(vid: u16, pid: u16) -> Result<Self> {
        Self::open_usb_with_serial(vid, pid, None)
    }

    /// Open a connection to a USB programmer by VID/PID and optional serial number
    ///
    /// # Arguments
    /// * `vid` - USB Vendor ID
    /// * `pid` - USB Product ID
    /// * `serial` - Optional serial number filter
    pub fn open_usb_with_serial(vid: u16, pid: u16, serial: Option<&str>) -> Result<Self> {
        let serial_owned = serial.map(String::from);

        // Create tokio runtime for async operations
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Usb(format!("Failed to create runtime: {}", e)))?;

        let client = HostClient::<ErrorResp>::try_new_raw_nusb(
            |d| {
                let vid_match = d.vendor_id() == vid;
                let pid_match = d.product_id() == pid;
                let serial_match = serial_owned
                    .as_ref()
                    .map(|s| d.serial_number() == Some(s.as_str()))
                    .unwrap_or(true);
                vid_match && pid_match && serial_match
            },
            "error", // Error topic path - not currently used but required
            8,       // Outgoing queue depth
            VarSeqKind::Seq1,
        )
        .map_err(Error::Usb)?;

        let mut programmer = Self {
            client,
            info: None,
            runtime,
        };

        // Query and cache device info
        programmer.info = Some(programmer.query_device_info()?);

        Ok(programmer)
    }

    /// Open a connection using the default VID/PID
    pub fn open_default() -> Result<Self> {
        Self::open_usb(USB_VID, USB_PID)
    }

    /// Query device information
    pub fn query_device_info(&mut self) -> Result<DeviceInfo> {
        self.runtime.block_on(async {
            let info = self
                .client
                .send_resp::<GetDeviceInfo>(&())
                .await
                .map_err(|e| Error::Protocol(format!("{:?}", e)))?;

            Ok(info)
        })
    }

    /// Get cached device info
    pub fn device_info(&self) -> Option<&DeviceInfo> {
        self.info.as_ref()
    }

    /// Set SPI frequency
    ///
    /// Returns the actual frequency set by the programmer (may differ from requested).
    pub fn set_spi_freq(&mut self, freq_hz: u32) -> Result<u32> {
        self.runtime.block_on(async {
            let resp = self
                .client
                .send_resp::<SetSpiFreqEp>(&SetSpiFreq { freq_hz })
                .await
                .map_err(|e| Error::Protocol(format!("{:?}", e)))?;

            match resp {
                Ok(r) => Ok(r.actual_freq),
                Err(e) => Err(Error::Device(e)),
            }
        })
    }

    /// Set chip select
    pub fn set_chip_select(&mut self, cs: u8) -> Result<()> {
        self.runtime.block_on(async {
            let resp = self
                .client
                .send_resp::<SetChipSelectEp>(&SetChipSelect { cs })
                .await
                .map_err(|e| Error::Protocol(format!("{:?}", e)))?;

            match resp {
                Ok(_) => Ok(()),
                Err(e) => Err(Error::Device(e)),
            }
        })
    }

    /// Enable or disable output drivers
    pub fn set_pin_state(&mut self, enabled: bool) -> Result<()> {
        self.runtime.block_on(async {
            let resp = self
                .client
                .send_resp::<SetPinStateEp>(&SetPinState { enabled })
                .await
                .map_err(|e| Error::Protocol(format!("{:?}", e)))?;

            match resp {
                Ok(_) => Ok(()),
                Err(e) => Err(Error::Device(e)),
            }
        })
    }

    /// Reset the SPI bus
    pub fn reset_spi(&mut self) -> Result<()> {
        self.runtime.block_on(async {
            let resp = self
                .client
                .send_resp::<SpiResetEp>(&())
                .await
                .map_err(|e| Error::Protocol(format!("{:?}", e)))?;

            match resp {
                Ok(_) => Ok(()),
                Err(e) => Err(Error::Device(e)),
            }
        })
    }

    /// Execute an SPI operation
    fn spi_op(&mut self, op: &SpiOp) -> Result<SpiOpResp> {
        self.runtime.block_on(async {
            let resp = self
                .client
                .send_resp::<SpiOpEp>(op)
                .await
                .map_err(|e| Error::Protocol(format!("{:?}", e)))?;

            match resp {
                Ok(r) => Ok(r),
                Err(e) => Err(Error::Device(e)),
            }
        })
    }

    /// Delay for the specified number of microseconds
    fn do_delay_us(&mut self, us: u32) -> Result<()> {
        self.runtime.block_on(async {
            self.client
                .send_resp::<DelayUsEp>(&DelayUs { us })
                .await
                .map_err(|e| Error::Protocol(format!("{:?}", e)))?;

            Ok(())
        })
    }

    /// Get the features supported by this programmer
    fn get_features(&self) -> SpiFeatures {
        if let Some(info) = &self.info {
            // 4-byte addressing is always supported
            let mut features = SpiFeatures::FOUR_BYTE_ADDR;

            if info
                .features
                .contains(crate::protocol::SpiFeatures::DUAL_IN)
            {
                features |= SpiFeatures::DUAL_IN;
            }
            if info
                .features
                .contains(crate::protocol::SpiFeatures::DUAL_IO)
            {
                features |= SpiFeatures::DUAL_IO;
            }
            if info
                .features
                .contains(crate::protocol::SpiFeatures::QUAD_IN)
            {
                features |= SpiFeatures::QUAD_IN;
            }
            if info
                .features
                .contains(crate::protocol::SpiFeatures::QUAD_IO)
            {
                features |= SpiFeatures::QUAD_IO;
            }
            if info.features.contains(crate::protocol::SpiFeatures::QPI) {
                features |= SpiFeatures::QPI;
            }

            features
        } else {
            // 4-byte addressing is always supported
            SpiFeatures::FOUR_BYTE_ADDR
        }
    }
}

impl Drop for PostcardProgrammer {
    fn drop(&mut self) {
        // Try to disable output drivers on cleanup
        let _ = self.set_pin_state(false);
    }
}

impl SpiMaster for PostcardProgrammer {
    fn features(&self) -> SpiFeatures {
        self.get_features()
    }

    fn max_read_len(&self) -> usize {
        self.info
            .as_ref()
            .map(|i| i.max_read_len as usize)
            .unwrap_or(MAX_SPI_DATA)
    }

    fn max_write_len(&self) -> usize {
        self.info
            .as_ref()
            .map(|i| i.max_write_len as usize)
            .unwrap_or(MAX_SPI_DATA)
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check that the requested I/O mode is supported
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // Convert address width
        let address_width = AddressWidth::from_core(cmd.address_width);

        // Convert I/O mode
        let io_mode = IoMode::from_core(cmd.io_mode);

        // Build write data
        let mut write_data = heapless::Vec::<u8, MAX_SPI_DATA>::new();
        for byte in cmd.write_data.iter() {
            write_data
                .push(*byte)
                .map_err(|_| CoreError::ProgrammerError)?;
        }

        // Build SPI operation
        let op = SpiOp {
            opcode: cmd.opcode,
            address: cmd.address.unwrap_or(0),
            address_width,
            io_mode,
            dummy_cycles: cmd.dummy_cycles,
            write_data,
            read_count: cmd.read_buf.len() as u16,
        };

        // Execute operation
        let resp = self.spi_op(&op).map_err(|_| CoreError::ProgrammerError)?;

        // Copy read data
        let read_len = resp.read_data.len().min(cmd.read_buf.len());
        cmd.read_buf[..read_len].copy_from_slice(&resp.read_data[..read_len]);

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        // Try to use device-side delay, fall back to host delay
        if self.do_delay_us(us).is_err() {
            std::thread::sleep(std::time::Duration::from_micros(us as u64));
        }
    }
}

/// List available postcard-rpc programmers
///
/// Returns a list of USB device info for devices matching the default VID/PID.
pub fn list_devices() -> Result<Vec<DeviceEntry>> {
    list_devices_with_vid_pid(USB_VID, USB_PID)
}

/// List available postcard-rpc programmers with specific VID/PID
pub fn list_devices_with_vid_pid(vid: u16, pid: u16) -> Result<Vec<DeviceEntry>> {
    let devices = nusb::list_devices().map_err(|e| Error::Usb(e.to_string()))?;

    let mut entries = Vec::new();
    for dev in devices {
        if dev.vendor_id() == vid && dev.product_id() == pid {
            entries.push(DeviceEntry {
                vid: dev.vendor_id(),
                pid: dev.product_id(),
                serial: dev.serial_number().map(String::from),
                manufacturer: dev.manufacturer_string().map(String::from),
                product: dev.product_string().map(String::from),
            });
        }
    }

    Ok(entries)
}

/// Information about an available USB device
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    /// USB Vendor ID
    pub vid: u16,
    /// USB Product ID
    pub pid: u16,
    /// Serial number (if available)
    pub serial: Option<String>,
    /// Manufacturer string (if available)
    pub manufacturer: Option<String>,
    /// Product string (if available)
    pub product: Option<String>,
}
