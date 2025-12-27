//! Linux MTD device implementation

use crate::error::{LinuxMtdError, Result};
use log::{debug, info, warn};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Sysfs root for MTD devices
const MTD_SYSFS_ROOT: &str = "/sys/class/mtd";

/// Device root
const DEV_ROOT: &str = "/dev";

/// MTD flags from kernel headers
mod mtd_flags {
    /// MTD device is writable
    pub const MTD_WRITEABLE: u64 = 0x400;
    /// MTD device doesn't require erase
    pub const MTD_NO_ERASE: u64 = 0x1000;
}

/// Configuration for opening a Linux MTD device
#[derive(Debug, Clone)]
pub struct LinuxMtdConfig {
    /// MTD device number (e.g., 0 for /dev/mtd0)
    pub dev_num: u32,
}

impl LinuxMtdConfig {
    /// Create a new configuration for the specified device number
    pub fn new(dev_num: u32) -> Self {
        Self { dev_num }
    }
}

/// Information about an MTD device read from sysfs
#[derive(Debug, Clone)]
pub struct MtdInfo {
    /// Device name from sysfs
    pub name: String,
    /// Total size in bytes
    pub total_size: u64,
    /// Erase block size in bytes
    pub erase_size: u64,
    /// Number of erase regions (must be 0 for uniform erase)
    pub num_erase_regions: u64,
    /// Whether the device is writable
    pub is_writable: bool,
    /// Whether the device requires erase before write
    pub requires_erase: bool,
}

/// Linux MTD device handle
///
/// This struct wraps a Linux MTD device and implements the OpaqueMaster trait.
/// Linux MTD provides high-level read/write/erase operations, abstracting
/// away the underlying flash protocol.
///
/// # Example
///
/// ```ignore
/// use rflasher_linux_mtd::{LinuxMtd, LinuxMtdConfig};
///
/// // Open MTD device 0
/// let mut mtd = LinuxMtd::open(&LinuxMtdConfig::new(0))?;
///
/// // Read flash contents
/// let mut buffer = vec![0u8; 4096];
/// mtd.read(0, &mut buffer)?;
/// ```
pub struct LinuxMtd {
    /// Device file handle
    file: File,
    /// Device information
    info: MtdInfo,
}

impl LinuxMtd {
    /// Open an MTD device by device number
    ///
    /// # Arguments
    /// * `config` - Configuration specifying the device number
    ///
    /// # Errors
    /// Returns an error if:
    /// - The device doesn't exist
    /// - The device is not a NOR flash device
    /// - The device has non-uniform erase regions
    /// - The device cannot be opened
    pub fn open(config: &LinuxMtdConfig) -> Result<Self> {
        let dev_num = config.dev_num;
        let sysfs_path = format!("{}/mtd{}", MTD_SYSFS_ROOT, dev_num);

        // Check if the device exists
        if !Path::new(&sysfs_path).exists() {
            return Err(LinuxMtdError::DeviceNotFound(format!(
                "MTD device {} not found ({})",
                dev_num, sysfs_path
            )));
        }

        // Check device type (must be "nor")
        let dev_type = read_sysfs_string(&sysfs_path, "type")?;
        if dev_type != "nor" {
            return Err(LinuxMtdError::NotNorFlash(format!(
                "MTD device {} type is '{}', expected 'nor'",
                dev_num, dev_type
            )));
        }

        // Read device information
        let info = read_mtd_info(&sysfs_path)?;

        debug!(
            "MTD{}: name='{}', size={}, erase_size={}, writable={}, requires_erase={}",
            dev_num,
            info.name,
            info.total_size,
            info.erase_size,
            info.is_writable,
            info.requires_erase
        );

        // Validate size is a power of 2
        if !info.total_size.is_power_of_two() {
            warn!(
                "MTD size {} is not a power of 2, some operations may fail",
                info.total_size
            );
        }

        // Validate erase size is a power of 2
        if !info.erase_size.is_power_of_two() {
            return Err(LinuxMtdError::InvalidEraseSize(info.erase_size));
        }

        // Non-uniform erase regions are not supported
        if info.num_erase_regions != 0 {
            return Err(LinuxMtdError::NonUniformEraseRegions(
                info.num_erase_regions,
            ));
        }

        // Open the device file
        let dev_path = format!("{}/mtd{}", DEV_ROOT, dev_num);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&dev_path)
            .map_err(|e| LinuxMtdError::SysfsRead {
                path: dev_path.clone(),
                source: e,
            })?;

        info!(
            "Opened {} successfully (name='{}', size={} bytes, erase_size={} bytes)",
            dev_path, info.name, info.total_size, info.erase_size
        );

        Ok(Self { file, info })
    }

    /// Get the device information
    pub fn info(&self) -> &MtdInfo {
        &self.info
    }

    /// Get the total flash size in bytes
    pub fn size(&self) -> u64 {
        self.info.total_size
    }

    /// Get the erase block size in bytes
    pub fn erase_size(&self) -> u64 {
        self.info.erase_size
    }
}

/// Read a string from a sysfs file and sanitize it
fn read_sysfs_string(sysfs_path: &str, filename: &str) -> Result<String> {
    let path = format!("{}/{}", sysfs_path, filename);
    let content = std::fs::read_to_string(&path).map_err(|e| LinuxMtdError::SysfsRead {
        path: path.clone(),
        source: e,
    })?;

    // Sanitize: remove non-printable characters and trailing whitespace
    let sanitized: String = content
        .chars()
        .take_while(|c| c.is_ascii_graphic() || *c == ' ')
        .collect();
    Ok(sanitized.trim_end().to_string())
}

/// Read an integer from a sysfs file
fn read_sysfs_int(sysfs_path: &str, filename: &str) -> Result<u64> {
    let value_str = read_sysfs_string(sysfs_path, filename)?;
    let path = format!("{}/{}", sysfs_path, filename);

    // Support hex (0x...) and decimal
    let value = if value_str.starts_with("0x") || value_str.starts_with("0X") {
        u64::from_str_radix(&value_str[2..], 16)
    } else {
        value_str.parse::<u64>()
    };

    value.map_err(|_| LinuxMtdError::SysfsParse {
        path,
        value: value_str,
    })
}

/// Read MTD device information from sysfs
fn read_mtd_info(sysfs_path: &str) -> Result<MtdInfo> {
    // Read flags
    let flags = read_sysfs_int(sysfs_path, "flags")?;
    let is_writable = (flags & mtd_flags::MTD_WRITEABLE) != 0;
    let requires_erase = (flags & mtd_flags::MTD_NO_ERASE) == 0;

    // Read name
    let name = read_sysfs_string(sysfs_path, "name")?;

    // Read size
    let total_size = read_sysfs_int(sysfs_path, "size")?;

    // Read erase size
    let erase_size = read_sysfs_int(sysfs_path, "erasesize")?;

    // Read number of erase regions
    let num_erase_regions = read_sysfs_int(sysfs_path, "numeraseregions")?;

    Ok(MtdInfo {
        name,
        total_size,
        erase_size,
        num_erase_regions,
        is_writable,
        requires_erase,
    })
}

/// MEMERASE ioctl argument structure
/// Matches struct erase_info_user from mtd/mtd-user.h
#[repr(C)]
struct EraseInfo {
    start: u32,
    length: u32,
}

// Generate the ioctl request code for MEMERASE
// MEMERASE = _IOW('M', 2, struct erase_info_user)
// Using nix ioctl macros
nix::ioctl_write_ptr!(memerase, b'M', 2, EraseInfo);

impl rflasher_core::programmer::OpaqueMaster for LinuxMtd {
    fn size(&self) -> usize {
        self.info.total_size as usize
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> rflasher_core::error::Result<()> {
        // Seek to the address
        self.file
            .seek(SeekFrom::Start(addr as u64))
            .map_err(|_| rflasher_core::error::Error::ReadError)?;

        // Read in chunks aligned to erase block size for better performance
        let eb_size = self.info.erase_size as usize;
        let mut offset = 0;

        while offset < buf.len() {
            // Try to align reads to eraseblock size
            let pos = addr as usize + offset;
            let step = std::cmp::min(eb_size - (pos % eb_size), buf.len() - offset);

            self.file
                .read_exact(&mut buf[offset..offset + step])
                .map_err(|_| rflasher_core::error::Error::ReadError)?;

            offset += step;
        }

        Ok(())
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> rflasher_core::error::Result<()> {
        if !self.info.is_writable {
            return Err(rflasher_core::error::Error::WriteProtected);
        }

        // Seek to the address
        self.file
            .seek(SeekFrom::Start(addr as u64))
            .map_err(|_| rflasher_core::error::Error::WriteError)?;

        // Write in chunks aligned to erase block size for better performance
        let chunksize = self.info.erase_size as usize;
        let mut offset = 0;

        while offset < data.len() {
            // Try to align writes to eraseblock size
            let pos = addr as usize + offset;
            let step = std::cmp::min(chunksize - (pos % chunksize), data.len() - offset);

            self.file
                .write_all(&data[offset..offset + step])
                .map_err(|_| rflasher_core::error::Error::WriteError)?;

            // Flush after each chunk
            self.file
                .flush()
                .map_err(|_| rflasher_core::error::Error::WriteError)?;

            offset += step;
        }

        Ok(())
    }

    fn erase(&mut self, addr: u32, len: u32) -> rflasher_core::error::Result<()> {
        if !self.info.requires_erase {
            // Device doesn't require erase (e.g., RAM-backed MTD)
            return Ok(());
        }

        if self.info.num_erase_regions != 0 {
            return Err(rflasher_core::error::Error::EraseError);
        }

        let erase_size = self.info.erase_size as u32;

        // Erase block by block
        let mut offset = 0u32;
        while offset < len {
            let erase_info = EraseInfo {
                start: addr + offset,
                length: erase_size,
            };

            // SAFETY: We're calling an ioctl with a valid file descriptor and
            // a properly initialized EraseInfo struct
            unsafe {
                memerase(self.file.as_raw_fd(), &erase_info)
                    .map_err(|_| rflasher_core::error::Error::EraseError)?;
            }

            offset += erase_size;
        }

        Ok(())
    }
}

/// Parse programmer options from key-value pairs
///
/// # Supported options
/// - `dev=N` - MTD device number (required)
///
/// # Example
/// ```ignore
/// let options = &[("dev", "0")];
/// let config = parse_options(options)?;
/// ```
pub fn parse_options(options: &[(&str, &str)]) -> Result<LinuxMtdConfig> {
    let mut dev_num: Option<u32> = None;

    for (key, value) in options {
        match *key {
            "dev" => {
                dev_num = Some(value.parse().map_err(|_| LinuxMtdError::InvalidParameter {
                    name: "dev",
                    message: format!("'{}' is not a valid device number", value),
                })?);
            }
            _ => {
                warn!("Unknown linux_mtd option: {}={}", key, value);
            }
        }
    }

    let dev_num = dev_num.ok_or(LinuxMtdError::MissingParameter("dev"))?;

    Ok(LinuxMtdConfig::new(dev_num))
}
