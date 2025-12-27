//! Linux SPI device implementation
//!
//! This module provides the `LinuxSpi` struct that implements the `SpiMaster`
//! trait using Linux's spidev interface.

use crate::error::{LinuxSpiError, Result};

use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;

/// Path to kernel spidev buffer size parameter
const BUF_SIZE_SYSFS: &str = "/sys/module/spidev/parameters/bufsiz";

/// Default SPI clock speed in Hz (2 MHz)
const DEFAULT_SPEED_HZ: u32 = 2_000_000;

/// SPI mode constants
pub mod mode {
    /// SPI mode 0: CPOL=0, CPHA=0
    pub const MODE_0: u8 = 0;
    /// SPI mode 1: CPOL=0, CPHA=1
    pub const MODE_1: u8 = 1;
    /// SPI mode 2: CPOL=1, CPHA=0
    pub const MODE_2: u8 = 2;
    /// SPI mode 3: CPOL=1, CPHA=1
    pub const MODE_3: u8 = 3;
}

/// Linux spidev ioctl constants
mod ioctl {
    use nix::ioctl_read;
    use nix::ioctl_write_ptr;

    // SPI ioctl magic number
    const SPI_IOC_MAGIC: u8 = b'k';

    // SPI ioctl type numbers
    const SPI_IOC_TYPE_MODE: u8 = 1;
    #[allow(dead_code)]
    const SPI_IOC_TYPE_LSB_FIRST: u8 = 2;
    const SPI_IOC_TYPE_BITS_PER_WORD: u8 = 3;
    const SPI_IOC_TYPE_MAX_SPEED_HZ: u8 = 4;

    // Generate ioctl functions
    ioctl_read!(spi_ioc_rd_mode, SPI_IOC_MAGIC, SPI_IOC_TYPE_MODE, u8);
    ioctl_write_ptr!(spi_ioc_wr_mode, SPI_IOC_MAGIC, SPI_IOC_TYPE_MODE, u8);
    ioctl_read!(
        spi_ioc_rd_lsb_first,
        SPI_IOC_MAGIC,
        SPI_IOC_TYPE_LSB_FIRST,
        u8
    );
    ioctl_write_ptr!(
        spi_ioc_wr_lsb_first,
        SPI_IOC_MAGIC,
        SPI_IOC_TYPE_LSB_FIRST,
        u8
    );
    ioctl_read!(
        spi_ioc_rd_bits_per_word,
        SPI_IOC_MAGIC,
        SPI_IOC_TYPE_BITS_PER_WORD,
        u8
    );
    ioctl_write_ptr!(
        spi_ioc_wr_bits_per_word,
        SPI_IOC_MAGIC,
        SPI_IOC_TYPE_BITS_PER_WORD,
        u8
    );
    ioctl_read!(
        spi_ioc_rd_max_speed_hz,
        SPI_IOC_MAGIC,
        SPI_IOC_TYPE_MAX_SPEED_HZ,
        u32
    );
    ioctl_write_ptr!(
        spi_ioc_wr_max_speed_hz,
        SPI_IOC_MAGIC,
        SPI_IOC_TYPE_MAX_SPEED_HZ,
        u32
    );

    // SPI_IOC_MESSAGE ioctl number calculation
    // This is SPI_IOC_MESSAGE(n) = _IOW(SPI_IOC_MAGIC, 0, char[SPI_MSGSIZE(n)])
    // where SPI_MSGSIZE(n) = (n) * sizeof(struct spi_ioc_transfer)
    // struct spi_ioc_transfer is 32 bytes on 64-bit and varies on 32-bit

    /// Size of spi_ioc_transfer struct (for 64-bit systems)
    pub const SPI_IOC_TRANSFER_SIZE: usize = 32;

    /// Calculate ioctl number for SPI_IOC_MESSAGE(n)
    pub fn spi_ioc_message(n: u8) -> libc::c_ulong {
        let size = (n as usize) * SPI_IOC_TRANSFER_SIZE;
        // _IOW = _IOC(_IOC_WRITE, type, nr, size)
        // _IOC_WRITE = 1
        // _IOC(dir, type, nr, size) = ((dir)<<30)|((size)<<16)|((type)<<8)|(nr)
        ((1u32 << 30) | ((size as u32) << 16) | ((SPI_IOC_MAGIC as u32) << 8)) as libc::c_ulong
    }
}

/// SPI transfer structure for ioctl
/// This must match the kernel's struct spi_ioc_transfer layout
#[repr(C)]
#[derive(Debug, Default, Clone)]
struct SpiIocTransfer {
    tx_buf: u64,          // __u64 tx_buf
    rx_buf: u64,          // __u64 rx_buf
    len: u32,             // __u32 len
    speed_hz: u32,        // __u32 speed_hz
    delay_usecs: u16,     // __u16 delay_usecs
    bits_per_word: u8,    // __u8 bits_per_word
    cs_change: u8,        // __u8 cs_change
    tx_nbits: u8,         // __u8 tx_nbits
    rx_nbits: u8,         // __u8 rx_nbits
    word_delay_usecs: u8, // __u8 word_delay_usecs
    _pad: u8,             // padding
}

/// Configuration for opening a Linux SPI device
#[derive(Debug, Clone)]
pub struct LinuxSpiConfig {
    /// Device path (e.g., "/dev/spidev0.0")
    pub device: String,
    /// SPI clock speed in Hz (default: 2 MHz)
    pub speed_hz: u32,
    /// SPI mode (0-3, default: 0)
    pub mode: u8,
}

impl Default for LinuxSpiConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            speed_hz: DEFAULT_SPEED_HZ,
            mode: mode::MODE_0,
        }
    }
}

impl LinuxSpiConfig {
    /// Create a new configuration with the given device path
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            ..Default::default()
        }
    }

    /// Set the SPI clock speed in Hz
    pub fn with_speed(mut self, speed_hz: u32) -> Self {
        self.speed_hz = speed_hz;
        self
    }

    /// Set the SPI mode (0-3)
    pub fn with_mode(mut self, mode: u8) -> Self {
        self.mode = mode;
        self
    }
}

/// Linux SPI programmer using spidev interface
///
/// This struct implements the `SpiMaster` trait for Linux systems using
/// the `/dev/spidevX.Y` device interface.
pub struct LinuxSpi {
    /// File handle for spidev device
    file: File,
    /// Maximum kernel buffer size
    max_kernel_buf_size: usize,
    /// Current speed in Hz
    speed_hz: u32,
}

impl LinuxSpi {
    /// Open a Linux SPI device with the given configuration
    pub fn open(config: &LinuxSpiConfig) -> Result<Self> {
        if config.device.is_empty() {
            return Err(LinuxSpiError::NoDevice);
        }

        log::debug!("linux_spi: Opening device {}", config.device);

        // Open the device
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&config.device)
            .map_err(|e| LinuxSpiError::OpenFailed {
                path: config.device.clone(),
                source: e,
            })?;

        let fd = file.as_raw_fd();

        // Set SPI mode
        let mode = config.mode;
        unsafe {
            ioctl::spi_ioc_wr_mode(fd, &mode).map_err(|e| LinuxSpiError::SetModeFailed {
                mode,
                source: std::io::Error::from_raw_os_error(e as i32),
            })?;
        }

        // Set bits per word (always 8)
        let bits: u8 = 8;
        unsafe {
            ioctl::spi_ioc_wr_bits_per_word(fd, &bits).map_err(|e| {
                LinuxSpiError::SetBitsPerWordFailed {
                    bits,
                    source: std::io::Error::from_raw_os_error(e as i32),
                }
            })?;
        }

        // Set clock speed
        let speed = config.speed_hz;
        unsafe {
            ioctl::spi_ioc_wr_max_speed_hz(fd, &speed).map_err(|e| {
                LinuxSpiError::SetSpeedFailed {
                    speed,
                    source: std::io::Error::from_raw_os_error(e as i32),
                }
            })?;
        }

        log::info!(
            "linux_spi: Opened {} (mode={}, speed={} kHz)",
            config.device,
            mode,
            speed / 1000
        );

        // Read max kernel buffer size
        let max_kernel_buf_size = get_max_kernel_buf_size();
        log::debug!(
            "linux_spi: Max kernel buffer size: {} bytes",
            max_kernel_buf_size
        );

        Ok(Self {
            file,
            max_kernel_buf_size,
            speed_hz: speed,
        })
    }

    /// Open a device with default settings
    pub fn open_device(device: &str) -> Result<Self> {
        Self::open(&LinuxSpiConfig::new(device))
    }

    /// Perform an SPI transfer
    ///
    /// This implements the SPI_IOC_MESSAGE ioctl with two transfers:
    /// 1. Write phase (transmit only)
    /// 2. Read phase (receive only)
    fn spi_transfer(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        let fd = self.file.as_raw_fd();

        // Prepare transfer structures
        // We use two transfers like the reference implementation:
        // - First transfer: write (tx_buf set, rx_buf null)
        // - Second transfer: read (tx_buf null, rx_buf set)

        if write_data.is_empty() {
            return Err(LinuxSpiError::InvalidParameter(
                "Write data cannot be empty".into(),
            ));
        }

        let transfers: Vec<SpiIocTransfer>;
        let num_transfers: u8;

        if read_buf.is_empty() {
            // Write-only transfer
            transfers = vec![SpiIocTransfer {
                tx_buf: write_data.as_ptr() as u64,
                rx_buf: 0,
                len: write_data.len() as u32,
                speed_hz: self.speed_hz,
                delay_usecs: 0,
                bits_per_word: 8,
                cs_change: 0,
                tx_nbits: 0,
                rx_nbits: 0,
                word_delay_usecs: 0,
                _pad: 0,
            }];
            num_transfers = 1;
        } else {
            // Write then read
            transfers = vec![
                SpiIocTransfer {
                    tx_buf: write_data.as_ptr() as u64,
                    rx_buf: 0,
                    len: write_data.len() as u32,
                    speed_hz: self.speed_hz,
                    delay_usecs: 0,
                    bits_per_word: 8,
                    cs_change: 0, // Keep CS asserted
                    tx_nbits: 0,
                    rx_nbits: 0,
                    word_delay_usecs: 0,
                    _pad: 0,
                },
                SpiIocTransfer {
                    tx_buf: 0,
                    rx_buf: read_buf.as_mut_ptr() as u64,
                    len: read_buf.len() as u32,
                    speed_hz: self.speed_hz,
                    delay_usecs: 0,
                    bits_per_word: 8,
                    cs_change: 0,
                    tx_nbits: 0,
                    rx_nbits: 0,
                    word_delay_usecs: 0,
                    _pad: 0,
                },
            ];
            num_transfers = 2;
        }

        // Perform ioctl
        let ioctl_num = ioctl::spi_ioc_message(num_transfers);
        let ret = unsafe { libc::ioctl(fd, ioctl_num, transfers.as_ptr()) };

        if ret < 0 {
            return Err(LinuxSpiError::TransferFailed(
                std::io::Error::last_os_error(),
            ));
        }

        Ok(())
    }

    /// Get current speed setting
    pub fn speed_hz(&self) -> u32 {
        self.speed_hz
    }

    /// Set a new SPI clock speed
    pub fn set_speed(&mut self, speed_hz: u32) -> Result<()> {
        let fd = self.file.as_raw_fd();
        unsafe {
            ioctl::spi_ioc_wr_max_speed_hz(fd, &speed_hz).map_err(|e| {
                LinuxSpiError::SetSpeedFailed {
                    speed: speed_hz,
                    source: std::io::Error::from_raw_os_error(e as i32),
                }
            })?;
        }
        self.speed_hz = speed_hz;
        log::debug!("linux_spi: Set speed to {} Hz", speed_hz);
        Ok(())
    }
}

impl SpiMaster for LinuxSpi {
    fn features(&self) -> SpiFeatures {
        // Linux spidev supports 4-byte addressing (handled in software)
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // Account for command + address overhead (5 bytes max)
        self.max_kernel_buf_size.saturating_sub(5)
    }

    fn max_write_len(&self) -> usize {
        // Account for command + address overhead (5 bytes max)
        self.max_kernel_buf_size.saturating_sub(5)
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check that the requested I/O mode is supported
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // Build the write data: opcode + address + dummy + write_data
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        // Encode opcode + address + dummy bytes
        cmd.encode_header(&mut write_data);

        // Append write data
        write_data[header_len..].copy_from_slice(cmd.write_data);

        // Perform SPI transfer
        self.spi_transfer(&write_data, cmd.read_buf)
            .map_err(|_| CoreError::ProgrammerError)?;

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

/// Read the maximum kernel buffer size from sysfs, or use page size as fallback
fn get_max_kernel_buf_size() -> usize {
    // Try to read from sysfs
    if let Ok(content) = std::fs::read_to_string(BUF_SIZE_SYSFS) {
        if let Ok(size) = content.trim().parse::<usize>() {
            if size > 0 {
                log::debug!("linux_spi: Using buffer size {} from sysfs", size);
                return size;
            }
        }
        log::warn!("linux_spi: Invalid buffer size in {}", BUF_SIZE_SYSFS);
    } else {
        log::debug!("linux_spi: Cannot read {}, using page size", BUF_SIZE_SYSFS);
    }

    // Fall back to page size
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    log::debug!("linux_spi: Using page size {} as buffer size", page_size);
    page_size
}

/// Parse programmer options from a list of key-value pairs
pub fn parse_options(options: &[(&str, &str)]) -> std::result::Result<LinuxSpiConfig, String> {
    let mut config = LinuxSpiConfig::default();

    for (key, value) in options {
        match *key {
            "dev" => {
                config.device = value.to_string();
            }
            "spispeed" => {
                // Parse speed in kHz
                let speed_khz: u32 = value
                    .parse()
                    .map_err(|_| format!("Invalid spispeed value: {}", value))?;
                config.speed_hz = speed_khz * 1000;
            }
            "mode" => {
                let mode: u8 = value
                    .parse()
                    .map_err(|_| format!("Invalid mode value: {}", value))?;
                if mode > 3 {
                    return Err(format!("Invalid SPI mode: {} (must be 0-3)", mode));
                }
                config.mode = mode;
            }
            _ => {
                log::warn!("linux_spi: Unknown option: {}={}", key, value);
            }
        }
    }

    if config.device.is_empty() {
        return Err("No device specified. Use dev=/dev/spidevX.Y".to_string());
    }

    Ok(config)
}
