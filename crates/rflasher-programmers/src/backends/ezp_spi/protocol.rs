//! EZP2013 / CORIGHT EP01 USB protocol definitions and packet builders

use rflasher_core::spi::opcodes;

use super::error::{EzpSpiError, Result};

// ---------------------------------------------------------------------------
// USB identifiers and endpoints
// ---------------------------------------------------------------------------

/// EZP2013 / CORIGHT EP01 USB Vendor ID
pub const EZP_VID: u16 = 0x0314;
/// EZP2013 / CORIGHT EP01 USB Product ID
pub const EZP_PID: u16 = 0x1011;
/// USB interface number
pub const EZP_INTERFACE: u8 = 0;
/// Bulk IN endpoint address
pub const EZP_EP_IN: u8 = 0x81;
/// Bulk OUT endpoint address
pub const EZP_EP_OUT: u8 = 0x02;

// ---------------------------------------------------------------------------
// Sizes and limits
// ---------------------------------------------------------------------------

/// USB Full-Speed bulk packet / command packet size
pub const EZP_PACKET_SIZE: usize = 64;
/// Maximum payload bytes per bulk transfer chunk (also firmware's write chunk size)
pub const EZP_MAX_CHUNK: usize = 2048;
/// Maximum bytes per single `CMD_READ` or `CMD_WRITE` command
pub const EZP_MAX_XFER: usize = 256 * 1024;
/// Firmware hardcoded page-program step size (bytes)
pub const EZP_PAGE_SIZE: usize = 256;
/// Maximum addressable flash range supported by this driver (16 MiB, 24-bit addresses)
pub const EZP_MAX_ADDR_SPACE: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Timing and SPI configuration
// ---------------------------------------------------------------------------

/// Per-USB-transfer timeout in milliseconds
pub const EZP_USB_TIMEOUT_MS: u64 = 2000;
/// Polling interval for `REQ_DONE` and `CMD_STATUS` in microseconds
pub const EZP_POLL_US: u32 = 10_000;
/// Wall-clock deadline waiting for `"AOK"` after `CMD_WRITE` in milliseconds
pub const EZP_DONE_TIMEOUT_MS: u64 = 500;
/// Wall-clock deadline waiting for block/sector erase completion in milliseconds (10 s)
pub const EZP_BLOCK_ERASE_TIMEOUT_MS: u64 = 10_000;
/// Wall-clock deadline waiting for chip erase completion in milliseconds (600 s)
pub const EZP_CHIP_ERASE_TIMEOUT_MS: u64 = 600_000;

/// SPI clock frequency in MHz (matches the vendor host's default `RDID` clock).
pub const EZP_SPI_MHZ: u8 = 8;
/// Supply voltage in tenths of a volt (`33` = 3.3 V; `0` stalls the firmware).
pub const EZP_VCC_3V3: u8 = 33;
/// Fixed supply voltage driven by the programmer in millivolts.
pub const EZP_SUPPLY_MV: u16 = 3300;

// ---------------------------------------------------------------------------
// Vendor control IN requests (bmRequestType = 0xC0, wValue = 0, wIndex = 0)
// ---------------------------------------------------------------------------

/// Check whether a write has finished (returns `"AOK"`)
pub const EZP_REQ_DONE: u8 = 0x41;
/// Check whether the programmer is ready before a command (returns `"FOK"`)
pub const EZP_REQ_READY: u8 = 0x46;
/// Read device type string (e.g. `"EP01 USB Programmer."`)
pub const EZP_REQ_TYPE: u8 = 0x54;
/// Read firmware version string (e.g. `"1.2.0.3"`)
pub const EZP_REQ_VERSION: u8 = 0x56;

// ---------------------------------------------------------------------------
// Bulk command bytes (pkt[0])
// ---------------------------------------------------------------------------

/// Read from flash (bulk IN data follows ACK)
pub const EZP_CMD_READ: u8 = 0x03;
/// Program flash (bulk OUT data follows ACK, then poll `REQ_DONE`)
pub const EZP_CMD_WRITE: u8 = 0x05;
/// Erase flash chip or OTP/security area (poll `CMD_STATUS` until IDLE)
pub const EZP_CMD_ERASE: u8 = 0x09;
/// Query busy/idle status (ACK byte 0 is `STATUS_IDLE` or `STATUS_BUSY`)
pub const EZP_CMD_STATUS: u8 = 0x20;
/// Read 3-byte JEDEC ID (returned in ACK bytes 0..3)
pub const EZP_CMD_RDID: u8 = 0x41;

/// `CMD_STATUS` response: programmer is idle
pub const EZP_STATUS_IDLE: u8 = 0x01;
/// `CMD_STATUS` response: programmer is busy (e.g. erasing)
pub const EZP_STATUS_BUSY: u8 = 0x02;

/// Device family selector for SPI NOR flash (`pkt[1]`)
pub const EZP_TYPE_SPI_FLASH: u8 = 0xF0;

/// Write mode: standard page program (256-byte step)
pub const EZP_WMODE_PAGE: u8 = 0;
/// Write mode: SST AAI word program (`0xAD`, 2 bytes per command)
pub const EZP_WMODE_AAI_WORD: u8 = 2;

// ---------------------------------------------------------------------------
// 64-byte bulk command packet layout
// ---------------------------------------------------------------------------

/// Command byte
pub const EZP_OFF_CMD: usize = 0;
/// Memory type (`EZP_TYPE_SPI_FLASH`)
pub const EZP_OFF_TYPE: usize = 1;
/// Bulk transfer chunk size (`le16`, `min(len, EZP_MAX_CHUNK)`)
pub const EZP_OFF_CHUNK: usize = 2;
/// End address (`le32`, `start + len`)
pub const EZP_OFF_END: usize = 4;
/// Start address (`le32`)
pub const EZP_OFF_START: usize = 8;
/// Supply voltage in 0.1 V units (`EZP_VCC_3V3`)
pub const EZP_OFF_VCC: usize = 12;
/// SPI clock in MHz (`EZP_SPI_MHZ`)
pub const EZP_OFF_SPI_MHZ: usize = 13;
/// Write mode (`le16`: `0` = page, `1` = AAI byte, `2` = AAI word)
pub const EZP_OFF_WMODE: usize = 14;
/// Target area (`0` = main array, `1` = OTP/security area)
pub const EZP_OFF_AREA: usize = 20;
/// Start of 16-byte SPI opcode table (`pkt[21..37]`)
pub const EZP_OFF_OPS: usize = 21;

// ---------------------------------------------------------------------------
// SPI opcode table indices (relative to EZP_OFF_OPS)
// ---------------------------------------------------------------------------

/// Total number of entries in the opcode table
pub const EZP_NUM_OPS: usize = 16;
/// Write Enable (`0x06` / `0x50`)
pub const EZP_OP_WREN: usize = 0;
/// Write Disable (`0x04`)
pub const EZP_OP_WRDI: usize = 1;
/// Read Data (`0x03`, or alternate read opcode)
pub const EZP_OP_READ: usize = 2;
/// Page / AAI Program (`0x02` / `0xAD`)
pub const EZP_OP_PROGRAM: usize = 3;
/// Chip Erase (`0xC7` / `0x60`)
pub const EZP_OP_CHIP_ERASE: usize = 4;
/// Read Status Register (`0x05`)
pub const EZP_OP_RDSR: usize = 5;
/// Write Status Register (`0x01`) — kept `0` in default table so firmware
/// never clears status protection bits automatically.
#[allow(dead_code)]
pub const EZP_OP_WRSR: usize = 6;
/// Read Extended Address Register (`0xC8` / `0x16`)
pub const EZP_OP_RDEAR: usize = 9;
/// Write Extended Address Register (`0xC5` / `0x17`)
pub const EZP_OP_WREAR: usize = 10;
/// Security/OTP area erase opcode slot
pub const EZP_OP_AREA_ERASE: usize = 15;

/// Default SPI NOR opcode table embedded in command packets.
///
/// `EZP_OP_WRSR` stays `0` so the firmware does not clear write-protection
/// bits behind the caller's back.
pub const EZP_DEFAULT_OPS: [u8; EZP_NUM_OPS] = {
    let mut ops = [0u8; EZP_NUM_OPS];
    ops[EZP_OP_WREN] = opcodes::WREN;
    ops[EZP_OP_WRDI] = opcodes::WRDI;
    ops[EZP_OP_READ] = opcodes::READ;
    ops[EZP_OP_PROGRAM] = opcodes::PP;
    ops[EZP_OP_CHIP_ERASE] = opcodes::CE_C7;
    ops[EZP_OP_RDSR] = opcodes::RDSR;
    ops
};

/// Build a 64-byte command packet initialized with default SPI NOR settings.
pub fn init_packet(cmd: u8, start: u32, len: u32) -> [u8; EZP_PACKET_SIZE] {
    let mut pkt = [0u8; EZP_PACKET_SIZE];
    pkt[EZP_OFF_CMD] = cmd;
    pkt[EZP_OFF_TYPE] = EZP_TYPE_SPI_FLASH;
    let chunk = (len as usize).min(EZP_MAX_CHUNK) as u16;
    pkt[EZP_OFF_CHUNK..EZP_OFF_CHUNK + 2].copy_from_slice(&chunk.to_le_bytes());
    let end = start.wrapping_add(len);
    pkt[EZP_OFF_END..EZP_OFF_END + 4].copy_from_slice(&end.to_le_bytes());
    pkt[EZP_OFF_START..EZP_OFF_START + 4].copy_from_slice(&start.to_le_bytes());
    pkt[EZP_OFF_VCC] = EZP_VCC_3V3;
    pkt[EZP_OFF_SPI_MHZ] = EZP_SPI_MHZ;
    pkt[EZP_OFF_WMODE] = EZP_WMODE_PAGE;
    pkt[EZP_OFF_OPS..EZP_OFF_OPS + EZP_NUM_OPS].copy_from_slice(&EZP_DEFAULT_OPS);
    pkt
}

/// Build the 64-byte `EZP_CMD_RDID` command packet.
pub fn rdid_packet() -> [u8; EZP_PACKET_SIZE] {
    let mut pkt = [0u8; EZP_PACKET_SIZE];
    pkt[EZP_OFF_CMD] = EZP_CMD_RDID;
    pkt[EZP_OFF_TYPE] = EZP_TYPE_SPI_FLASH;
    pkt[EZP_OFF_VCC] = EZP_VCC_3V3;
    pkt[EZP_OFF_SPI_MHZ] = EZP_SPI_MHZ;
    pkt
}

/// Build the 64-byte `EZP_CMD_STATUS` command packet.
pub fn status_packet() -> [u8; EZP_PACKET_SIZE] {
    let mut pkt = [0u8; EZP_PACKET_SIZE];
    pkt[EZP_OFF_CMD] = EZP_CMD_STATUS;
    pkt
}

/// Check without integer overflow whether `[start, start + len)` lies within
/// the 16 MiB address space supported by this backend.
pub const fn in_range(start: u32, len: usize) -> bool {
    len <= EZP_MAX_ADDR_SPACE && (start as usize) <= EZP_MAX_ADDR_SPACE - len
}

/// Whether `len` is valid for a single `EZP_CMD_WRITE` command.
///
/// Payloads above `EZP_MAX_CHUNK` (2048 bytes) must be exact multiples of
/// `EZP_MAX_CHUNK` because the firmware expects fixed-size chunks.
pub const fn is_valid_write_len(len: usize) -> bool {
    len > 0 && len <= EZP_MAX_XFER && (len <= EZP_MAX_CHUNK || len.is_multiple_of(EZP_MAX_CHUNK))
}

/// Longest length `<= len` accepted by `ezp_write` and aligned to `align`.
pub const fn ezp_write_len(len: usize, align: usize) -> usize {
    let clamped = if len > EZP_MAX_XFER {
        EZP_MAX_XFER
    } else {
        len
    };
    let step = if clamped > EZP_MAX_CHUNK {
        EZP_MAX_CHUNK
    } else if align == 0 {
        1
    } else {
        align
    };
    (clamped / step) * step
}

/// Choose the next chunk length for page-program mode (`EZP_WMODE_PAGE`).
///
/// The firmware advances by 256 bytes per page-program SPI frame:
/// - For `page_size < 256` or non-256-multiple page sizes, cap each transfer
///   to the current chip page (and `<= 256` bytes) to prevent page wrap.
/// - For 256-byte-multiple page sizes (`>= 256`), align heads/tails to 256
///   bytes and stream aligned runs up to 256 KiB while satisfying
///   [`is_valid_write_len`].
pub const fn page_program_chunk_len(start: u32, remaining: usize, page_size: usize) -> usize {
    if remaining == 0 || page_size == 0 {
        return 0;
    }

    if page_size < EZP_PAGE_SIZE || !page_size.is_multiple_of(EZP_PAGE_SIZE) {
        let page_off = (start as usize) % page_size;
        let to_page_end = page_size - page_off;
        let capped = if remaining < to_page_end {
            remaining
        } else {
            to_page_end
        };
        if capped < EZP_PAGE_SIZE {
            capped
        } else {
            EZP_PAGE_SIZE
        }
    } else {
        let page_off = (start as usize) % EZP_PAGE_SIZE;
        if page_off != 0 || remaining < EZP_PAGE_SIZE {
            let to_page_end = EZP_PAGE_SIZE - page_off;
            if remaining < to_page_end {
                remaining
            } else {
                to_page_end
            }
        } else {
            ezp_write_len(remaining, EZP_PAGE_SIZE)
        }
    }
}

/// Decode a NUL-terminated (or raw) ASCII/UTF-8 control response string.
pub fn decode_c_string(bytes: &[u8]) -> String {
    let trimmed = bytes.split(|&b| b == 0).next().unwrap_or(&[]);
    String::from_utf8_lossy(trimmed).into_owned()
}

/// Validate the device type and firmware version returned at open time.
pub fn validate_identity(device_type: &str, firmware_version: &str) -> Result<()> {
    if device_type.contains("BOOTLOADER") {
        return Err(EzpSpiError::BootloaderMode(device_type.to_string()));
    }
    if !firmware_version.starts_with("1.2.0.") {
        return Err(EzpSpiError::UnsupportedFirmware {
            device_type: device_type.to_string(),
            version: firmware_version.to_string(),
        });
    }
    Ok(())
}
