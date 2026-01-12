//! Serprog device implementation
//!
//! This module provides the main `Serprog` struct that implements the
//! serprog protocol and the `SpiMaster` trait.
//! Uses `maybe_async` to support both sync and async modes.

use crate::error::{Result, SerprogError};
use crate::protocol::*;
use crate::transport::Transport;

use maybe_async::maybe_async;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

/// Serprog programmer
///
/// This struct represents a connection to a serprog device and implements
/// the `SpiMaster` trait for communicating with SPI flash chips.
pub struct Serprog<T: Transport> {
    /// Transport layer (serial or TCP)
    transport: T,
    /// Programmer capabilities
    info: ProgrammerInfo,
    /// Whether automatic command checking is enabled
    auto_check: bool,
}

impl<T: Transport> Serprog<T> {
    /// Create a new Serprog instance with the given transport
    ///
    /// This performs initialization:
    /// 1. Synchronize the protocol
    /// 2. Query interface version
    /// 3. Query command map
    /// 4. Query programmer capabilities
    #[maybe_async]
    pub async fn new(transport: T) -> Result<Self> {
        let mut serprog = Self {
            transport,
            info: ProgrammerInfo::default(),
            auto_check: false,
        };

        // Synchronize protocol
        serprog.synchronize().await?;
        log::debug!("serprog: Synchronized");

        // Query interface version
        let version = serprog.query_iface().await?;
        if version != SERPROG_PROTOCOL_VERSION {
            return Err(SerprogError::UnsupportedVersion(version));
        }
        log::debug!("serprog: Interface version OK ({})", version);

        // Query command map
        serprog.info.cmdmap = serprog.query_cmdmap().await?;
        serprog.auto_check = true;

        // Query bus types
        serprog.info.bustypes = serprog.query_bustype().await.unwrap_or(bus::NONSPI);
        log::debug!(
            "serprog: Bus support: parallel={}, LPC={}, FWH={}, SPI={}",
            (serprog.info.bustypes & bus::PARALLEL) != 0,
            (serprog.info.bustypes & bus::LPC) != 0,
            (serprog.info.bustypes & bus::FWH) != 0,
            (serprog.info.bustypes & bus::SPI) != 0
        );

        // Check SPI support
        if !serprog.info.supports_spi() {
            return Err(SerprogError::SpiNotSupported);
        }

        // Check O_SPIOP support (required for SPI)
        if !serprog.info.supports_cmd(S_CMD_O_SPIOP) {
            log::error!("serprog: SPI operation not supported while bustype is SPI");
            return Err(SerprogError::CommandNotSupported(S_CMD_O_SPIOP));
        }

        // Set bus type to SPI
        let bt = bus::SPI;
        serprog.do_command(S_CMD_S_BUSTYPE, &[bt], &mut []).await?;

        // Query optional parameters
        if let Ok(buf) = serprog.do_command_ret::<3>(S_CMD_Q_WRNMAXLEN).await {
            serprog.info.max_write_n = u24_to_u32(&buf);
            log::debug!(
                "serprog: Maximum write-n length is {}",
                serprog.info.effective_max_write()
            );
        }

        if let Ok(buf) = serprog.do_command_ret::<3>(S_CMD_Q_RDNMAXLEN).await {
            serprog.info.max_read_n = u24_to_u32(&buf);
            log::debug!(
                "serprog: Maximum read-n length is {}",
                serprog.info.effective_max_read()
            );
        }

        // Query programmer name
        if let Ok(name) = serprog.do_command_ret::<16>(S_CMD_Q_PGMNAME).await {
            serprog.info.name = name;
            log::info!(
                "serprog: Programmer name is \"{}\"",
                serprog.info.name_str()
            );
        }

        // Query serial buffer size
        if let Ok(buf) = serprog.do_command_ret::<2>(S_CMD_Q_SERBUF).await {
            serprog.info.serbuf_size = u16::from_le_bytes(buf);
            log::debug!(
                "serprog: Serial buffer size is {}",
                serprog.info.serbuf_size
            );
        }

        // Enable output drivers if supported
        if serprog.info.supports_cmd(S_CMD_S_PIN_STATE)
            && serprog
                .do_command(S_CMD_S_PIN_STATE, &[1], &mut [])
                .await
                .is_ok()
        {
            log::debug!("serprog: Output drivers enabled");
        }

        // Set bus type to all supported types
        let bt = serprog.info.bustypes;
        let _ = serprog.do_command(S_CMD_S_BUSTYPE, &[bt], &mut []).await;

        Ok(serprog)
    }

    /// Set the SPI clock frequency in Hz
    ///
    /// Returns the actual frequency set by the programmer.
    #[maybe_async]
    pub async fn set_spi_speed(&mut self, freq_hz: u32) -> Result<u32> {
        if !self.info.supports_cmd(S_CMD_S_SPI_FREQ) {
            log::warn!("serprog: Setting SPI clock rate is not supported");
            return Err(SerprogError::CommandNotSupported(S_CMD_S_SPI_FREQ));
        }

        let freq_bytes = freq_hz.to_le_bytes();
        let mut ret_buf = [0u8; 4];
        self.do_command(S_CMD_S_SPI_FREQ, &freq_bytes, &mut ret_buf)
            .await?;

        let actual_freq = u32::from_le_bytes(ret_buf);
        log::info!(
            "serprog: Requested SPI frequency {} Hz, set to {} Hz",
            freq_hz,
            actual_freq
        );

        Ok(actual_freq)
    }

    /// Set which SPI chip select to use (0-255)
    #[maybe_async]
    pub async fn set_spi_cs(&mut self, cs: u8) -> Result<()> {
        if !self.info.supports_cmd(S_CMD_S_SPI_CS) {
            return Err(SerprogError::CommandNotSupported(S_CMD_S_SPI_CS));
        }

        self.do_command(S_CMD_S_SPI_CS, &[cs], &mut []).await?;
        log::debug!("serprog: Using chip select {}", cs);

        Ok(())
    }

    /// Get programmer information
    pub fn info(&self) -> &ProgrammerInfo {
        &self.info
    }

    /// Perform an SPI operation
    ///
    /// This is the core function for SPI communication, implementing S_CMD_O_SPIOP.
    #[maybe_async]
    pub async fn spi_op(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        let writecnt = write_data.len();
        let readcnt = read_buf.len();

        // Build parameter buffer: 3 bytes write count + 3 bytes read count + write data
        let mut params = Vec::with_capacity(6 + writecnt);
        params.push((writecnt & 0xFF) as u8);
        params.push(((writecnt >> 8) & 0xFF) as u8);
        params.push(((writecnt >> 16) & 0xFF) as u8);
        params.push((readcnt & 0xFF) as u8);
        params.push(((readcnt >> 8) & 0xFF) as u8);
        params.push(((readcnt >> 16) & 0xFF) as u8);
        params.extend_from_slice(write_data);

        self.do_command(S_CMD_O_SPIOP, &params, read_buf).await?;

        Ok(())
    }

    /// Disable output drivers (called on drop in sync mode)
    #[maybe_async]
    pub async fn shutdown(&mut self) {
        // Disable output drivers if supported
        if self.info.supports_cmd(S_CMD_S_PIN_STATE) {
            if self
                .do_command(S_CMD_S_PIN_STATE, &[0], &mut [])
                .await
                .is_ok()
            {
                log::debug!("serprog: Output drivers disabled");
            }
        }
    }

    // ---- Protocol implementation ----

    /// Synchronize the protocol
    ///
    /// This brings the serial protocol to a known waiting-for-command state.
    #[maybe_async]
    async fn synchronize(&mut self) -> Result<()> {
        // Try a simple test first
        if self.test_sync().await? {
            return Ok(());
        }

        log::debug!("serprog: Attempting to synchronize");

        // Send 8 NOPs to reset the parser state
        let nops = [S_CMD_NOP; 8];
        if !self.transport.write_nonblock(&nops, 1).await? {
            return Err(SerprogError::SyncFailed);
        }

        // Drain any pending data
        let mut buf = [0u8; 512];
        for _ in 0..1024 {
            let n = self.transport.read_nonblock(&mut buf, 10).await?;
            if n == 0 {
                break;
            }
        }

        // Try sync again up to 8 times
        for _ in 0..8 {
            if self.test_sync().await? {
                return Ok(());
            }
        }

        Err(SerprogError::SyncFailed)
    }

    /// Test synchronization by sending SYNCNOP
    ///
    /// Returns true if synchronized, false if not.
    #[maybe_async]
    async fn test_sync(&mut self) -> Result<bool> {
        // Send SYNCNOP
        if !self.transport.write_nonblock(&[S_CMD_SYNCNOP], 1).await? {
            return Err(SerprogError::IoError("Write failed".into()));
        }

        // Try to read NAK
        let mut c = [0u8];
        for _ in 0..10 {
            let n = self.transport.read_nonblock(&mut c, 50).await?;
            if n == 0 || c[0] != S_NAK {
                continue;
            }

            // Got NAK, now expect ACK
            let n = self.transport.read_nonblock(&mut c, 20).await?;
            if n == 0 || c[0] != S_ACK {
                continue;
            }

            // Send another SYNCNOP to confirm
            if !self.transport.write_nonblock(&[S_CMD_SYNCNOP], 1).await? {
                return Err(SerprogError::IoError("Write failed".into()));
            }

            let n = self.transport.read_nonblock(&mut c, 500).await?;
            if n == 0 || c[0] != S_NAK {
                return Ok(false);
            }

            let n = self.transport.read_nonblock(&mut c, 100).await?;
            if n == 0 || c[0] != S_ACK {
                return Ok(false);
            }

            return Ok(true);
        }

        Ok(false)
    }

    /// Execute a serprog command
    #[maybe_async]
    async fn do_command(&mut self, cmd: u8, params: &[u8], ret_buf: &mut [u8]) -> Result<()> {
        // Check command availability
        if self.auto_check && !self.info.supports_cmd(cmd) {
            log::debug!("serprog: Command 0x{:02X} not supported", cmd);
            return Err(SerprogError::CommandNotSupported(cmd));
        }

        // Send command
        self.transport.write(&[cmd]).await?;

        // Send parameters
        if !params.is_empty() {
            self.transport.write(params).await?;
        }

        // Read response
        let mut response = [0u8];
        self.transport.read(&mut response).await?;

        if response[0] == S_NAK {
            return Err(SerprogError::Nak(cmd));
        }

        if response[0] != S_ACK {
            return Err(SerprogError::InvalidResponse {
                command: cmd,
                response: response[0],
            });
        }

        // Read return data
        if !ret_buf.is_empty() {
            self.transport.read(ret_buf).await?;
        }

        Ok(())
    }

    /// Execute a command and return the result in a fixed-size array
    #[maybe_async]
    async fn do_command_ret<const N: usize>(&mut self, cmd: u8) -> Result<[u8; N]> {
        let mut buf = [0u8; N];
        self.do_command(cmd, &[], &mut buf).await?;
        Ok(buf)
    }

    /// Query interface version
    #[maybe_async]
    async fn query_iface(&mut self) -> Result<u16> {
        let mut buf = [0u8; 2];
        // Don't use auto_check for Q_IFACE
        let saved = self.auto_check;
        self.auto_check = false;
        let result = self.do_command(S_CMD_Q_IFACE, &[], &mut buf).await;
        self.auto_check = saved;
        result?;
        Ok(u16::from_le_bytes(buf))
    }

    /// Query command map
    #[maybe_async]
    async fn query_cmdmap(&mut self) -> Result<CommandMap> {
        let mut cmdmap = CommandMap::new();
        // Don't use auto_check for Q_CMDMAP
        let saved = self.auto_check;
        self.auto_check = false;
        self.do_command(S_CMD_Q_CMDMAP, &[], &mut cmdmap.bitmap)
            .await?;
        self.auto_check = saved;
        Ok(cmdmap)
    }

    /// Query bus type
    #[maybe_async]
    async fn query_bustype(&mut self) -> Result<u8> {
        let buf = self.do_command_ret::<1>(S_CMD_Q_BUSTYPE).await?;
        Ok(buf[0])
    }
}

// Drop implementation only for sync mode (async requires explicit shutdown)
#[cfg(feature = "is_sync")]
impl<T: Transport> Drop for Serprog<T> {
    fn drop(&mut self) {
        // Disable output drivers if supported
        if self.info.supports_cmd(S_CMD_S_PIN_STATE)
            && self.do_command(S_CMD_S_PIN_STATE, &[0], &mut []).is_ok()
        {
            log::debug!("serprog: Output drivers disabled");
        }
    }
}

#[maybe_async(AFIT)]
impl<T: Transport> SpiMaster for Serprog<T> {
    fn features(&self) -> SpiFeatures {
        // Serprog supports 4-byte addressing (handled in software)
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        self.info.effective_max_read()
    }

    fn max_write_len(&self) -> usize {
        self.info.effective_max_write()
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check that the requested I/O mode is supported
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // Build the write data: opcode + address + dummy + write_data
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        // Encode opcode + address + dummy bytes
        cmd.encode_header(&mut write_data);

        // Append write data
        write_data[header_len..].copy_from_slice(cmd.write_data);

        // Perform SPI operation
        self.spi_op(&write_data, cmd.read_buf)
            .await
            .map_err(|_| CoreError::ProgrammerError)?;

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        // For serprog, we just use a standard delay
        // The protocol has O_DELAY but it's for the operation buffer (non-SPI)
        #[cfg(feature = "is_sync")]
        {
            #[cfg(feature = "std")]
            std::thread::sleep(std::time::Duration::from_micros(us as u64));
        }

        #[cfg(not(feature = "is_sync"))]
        {
            // In async mode, we need an async sleep
            // This will be provided by the runtime (tokio, wasm, etc.)
            // For now, just a no-op placeholder - actual implementations
            // should provide a proper async delay
            let _ = us;
        }
    }
}

/// Convert a 24-bit little-endian value to u32
fn u24_to_u32(buf: &[u8; 3]) -> u32 {
    (buf[0] as u32) | ((buf[1] as u32) << 8) | ((buf[2] as u32) << 16)
}
