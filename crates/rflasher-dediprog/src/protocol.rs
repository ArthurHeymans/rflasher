//! Dediprog protocol constants and types
//!
//! Based on the Dediprog SF100/SF200/SF600/SF700 protocol as implemented in flashprog.
//! The protocol uses USB control transfers for commands and bulk transfers for data.

#![allow(dead_code)]

// USB device identifiers
pub const DEDIPROG_USB_VENDOR: u16 = 0x0483;
pub const DEDIPROG_USB_PRODUCT: u16 = 0xDADA;

// USB endpoints (SF200 and older use EP2 for both, SF600+ uses EP1 for out)
pub const BULK_IN_EP: u8 = 0x82; // EP2 IN for all devices
pub const BULK_OUT_EP_SF100: u8 = 0x02; // EP2 OUT for SF100/SF200
pub const BULK_OUT_EP_SF600: u8 = 0x01; // EP1 OUT for SF600/SF700

// USB request types
pub const REQTYPE_EP_IN: u8 = 0xC2; // LIBUSB_ENDPOINT_IN | LIBUSB_REQUEST_TYPE_VENDOR | LIBUSB_RECIPIENT_ENDPOINT
pub const REQTYPE_EP_OUT: u8 = 0x42; // LIBUSB_ENDPOINT_OUT | LIBUSB_REQUEST_TYPE_VENDOR | LIBUSB_RECIPIENT_ENDPOINT
pub const REQTYPE_OTHER_IN: u8 = 0xC3;
pub const REQTYPE_OTHER_OUT: u8 = 0x43;

// Protocol timeouts
pub const DEFAULT_TIMEOUT_MS: u64 = 3000;
pub const ASYNC_TIMEOUT_SECS: u64 = 10;

// Transfer limits
pub const MAX_BLOCK_COUNT: u16 = 65535;
pub const MAX_CMD_SIZE: usize = 15;
pub const ASYNC_TRANSFERS: usize = 8;
pub const BULK_CHUNK_SIZE: usize = 512;

/// Firmware version encoding (same as flashprog)
#[inline]
pub const fn firmware_version(major: u32, minor: u32, patch: u32) -> u32 {
    (major << 16) | (minor << 8) | patch
}

/// Dediprog device type
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceType {
    Unknown,
    SF100,
    SF200,
    SF600,
    SF600PG2,
    SF700,
}

impl DeviceType {
    /// Parse device type from device string
    pub fn from_device_string(s: &str) -> Self {
        if s.starts_with("SF700") {
            DeviceType::SF700
        } else if s.starts_with("SF600PG2") {
            DeviceType::SF600PG2
        } else if s.starts_with("SF600") {
            DeviceType::SF600
        } else if s.starts_with("SF200") {
            DeviceType::SF200
        } else if s.starts_with("SF100") {
            DeviceType::SF100
        } else {
            DeviceType::Unknown
        }
    }

    /// Device family (100s)
    pub fn family(&self) -> u32 {
        match self {
            DeviceType::Unknown => 0,
            DeviceType::SF100 => 100,
            DeviceType::SF200 => 200,
            DeviceType::SF600 => 600,
            DeviceType::SF600PG2 => 600,
            DeviceType::SF700 => 700,
        }
    }

    /// Is this an SF600 class device (SF600, SF600PG2, SF700)?
    pub fn is_sf600_class(&self) -> bool {
        matches!(
            self,
            DeviceType::SF600 | DeviceType::SF600PG2 | DeviceType::SF700
        )
    }
}

impl std::fmt::Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceType::Unknown => write!(f, "Unknown"),
            DeviceType::SF100 => write!(f, "SF100"),
            DeviceType::SF200 => write!(f, "SF200"),
            DeviceType::SF600 => write!(f, "SF600"),
            DeviceType::SF600PG2 => write!(f, "SF600PG2"),
            DeviceType::SF700 => write!(f, "SF700"),
        }
    }
}

/// Protocol version (affects command encoding)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Protocol {
    Unknown,
    V1, // Original protocol (SF100/SF200 with older firmware)
    V2, // Extended protocol (SF100/SF200 v5.5+, SF600 v6.9+)
    V3, // Latest protocol (SF600 v7.2.22+, SF600PG2, SF700)
}

impl Protocol {
    /// Determine protocol version based on device type and firmware version
    pub fn from_device_firmware(device_type: DeviceType, firmware: u32) -> Self {
        match device_type {
            DeviceType::SF100 | DeviceType::SF200 => {
                if firmware < firmware_version(5, 5, 0) {
                    Protocol::V1
                } else {
                    Protocol::V2
                }
            }
            DeviceType::SF600 => {
                if firmware < firmware_version(6, 9, 0) {
                    Protocol::V1
                } else if firmware <= firmware_version(7, 2, 21) {
                    Protocol::V2
                } else {
                    Protocol::V3
                }
            }
            DeviceType::SF700 | DeviceType::SF600PG2 => Protocol::V3,
            DeviceType::Unknown => Protocol::Unknown,
        }
    }
}

/// USB commands for the Dediprog protocol
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Transceive = 0x01,
    PollStatusReg = 0x02,
    SetVpp = 0x03,
    SetTarget = 0x04,
    ReadEeprom = 0x05,
    WriteEeprom = 0x06,
    SetIoLed = 0x07,
    ReadProgInfo = 0x08,
    SetVcc = 0x09,
    SetStandalone = 0x0A,
    SetVoltage = 0x0B, // Only in firmware older than 6.0.0
    GetButton = 0x11,
    GetUid = 0x12,
    SetCs = 0x14,
    IoMode = 0x15,
    FwUpdate = 0x1A,
    FpgaUpdate = 0x1B,
    ReadFpgaVersion = 0x1C,
    SetHold = 0x1D,
    Read = 0x20,
    Write = 0x30,
    WriteAt45db = 0x31,
    NandWrite = 0x32,
    NandRead = 0x33,
    SetSpiClk = 0x61,
    CheckSocket = 0x62,
    DownloadPrj = 0x63,
    ReadPrjName = 0x64,
    // New protocol/firmware only
    CheckSdcard = 0x65,
    ReadPrj = 0x66,
}

/// LED states
#[repr(i8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Led {
    Invalid = -1,
    None = 0,
    Pass = 1,
    Busy = 2,
    Error = 4,
    All = 7,
}

/// Target flash type
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    ApplicationFlash1 = 0,
    FlashCard = 1,
    ApplicationFlash2 = 2,
    Socket = 3,
}

impl Target {
    /// Parse target from string/number
    pub fn from_value(v: u8) -> Option<Self> {
        match v {
            0 => Some(Target::ApplicationFlash1),
            1 => Some(Target::FlashCard),
            2 => Some(Target::ApplicationFlash2),
            3 => Some(Target::Socket),
            _ => None,
        }
    }
}

/// Read mode for bulk reads
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    Std = 1,
    Fast = 2,
    Atmel45 = 3,
    FourByteAddrFast = 4,
    FourByteAddrFast0x0C = 5, // New protocol only
    Configurable = 9,         // Not seen documented
}

/// Write mode for bulk writes
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    PagePgm = 1,
    PageWrite = 2,
    OneByteAai = 3,
    TwoByteAai = 4,
    Page128B = 5,
    PageAt26df041 = 6,
    SiliconBlueFpga = 7,
    Page64BNumonyxPcm = 8,
    FourByteAddr256BPagePgm = 9,
    Page32BMxic512k = 10,
    FourByteAddr256BPagePgm0x12 = 11,
    FourByteAddr256BPagePgmFlags = 12,
}

/// Standalone mode commands
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandaloneMode {
    Enter = 0,
    Leave = 1,
}

/// Dediprog I/O mode for multi-I/O commands
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpIoMode {
    Single = 0,
    DualOut = 1,
    DualIo = 2,
    QuadOut = 3,
    QuadIo = 4,
    Qpi = 5,
}

impl From<rflasher_core::spi::IoMode> for DpIoMode {
    fn from(mode: rflasher_core::spi::IoMode) -> Self {
        use rflasher_core::spi::IoMode;
        match mode {
            IoMode::Single => DpIoMode::Single,
            IoMode::DualOut => DpIoMode::DualOut,
            IoMode::DualIo => DpIoMode::DualIo,
            IoMode::QuadOut => DpIoMode::QuadOut,
            IoMode::QuadIo => DpIoMode::QuadIo,
            IoMode::Qpi => DpIoMode::Qpi,
        }
    }
}

/// SPI speed settings
#[derive(Debug, Clone, Copy)]
pub struct SpiSpeed {
    pub name: &'static str,
    pub value: u8,
}

/// Available SPI speeds
pub const SPI_SPEEDS: &[SpiSpeed] = &[
    SpiSpeed {
        name: "24M",
        value: 0x0,
    },
    SpiSpeed {
        name: "12M",
        value: 0x2,
    },
    SpiSpeed {
        name: "8M",
        value: 0x1,
    },
    SpiSpeed {
        name: "3M",
        value: 0x3,
    },
    SpiSpeed {
        name: "2.18M",
        value: 0x4,
    },
    SpiSpeed {
        name: "1.5M",
        value: 0x5,
    },
    SpiSpeed {
        name: "750k",
        value: 0x6,
    },
    SpiSpeed {
        name: "375k",
        value: 0x7,
    },
];

/// Default SPI speed index (12MHz)
pub const DEFAULT_SPI_SPEED_INDEX: usize = 1;

/// Voltage settings
#[derive(Debug, Clone, Copy)]
pub struct VoltageSelector {
    pub millivolt: u16,
    pub value: u16,
}

/// Available voltage settings
pub const VOLTAGES: &[VoltageSelector] = &[
    VoltageSelector {
        millivolt: 0,
        value: 0x0,
    },
    VoltageSelector {
        millivolt: 1800,
        value: 0x12,
    },
    VoltageSelector {
        millivolt: 2500,
        value: 0x11,
    },
    VoltageSelector {
        millivolt: 3500,
        value: 0x10,
    },
];

/// Default voltage (3.5V)
pub const DEFAULT_VOLTAGE_MV: u16 = 3500;

/// Get voltage selector value for a given millivolt setting
pub fn voltage_selector(millivolt: u16) -> Option<u16> {
    VOLTAGES
        .iter()
        .find(|v| v.millivolt == millivolt)
        .map(|v| v.value)
}

/// Parse SPI speed from string
pub fn parse_spi_speed(s: &str) -> Option<usize> {
    SPI_SPEEDS
        .iter()
        .position(|speed| speed.name.eq_ignore_ascii_case(s))
}

/// Parse voltage from string (e.g., "3.5V", "1800mV", "1.8")
pub fn parse_voltage(s: &str) -> Option<u16> {
    let s = s.trim().to_lowercase();

    // Try parsing with units
    if let Some(mv) = s.strip_suffix("mv") {
        return mv.trim().parse().ok();
    }
    if let Some(v) = s.strip_suffix('v') {
        let parsed: f32 = v.trim().parse().ok()?;
        return Some((parsed * 1000.0) as u16);
    }

    // Try parsing as a number (assume volts if > 100, else millivolts)
    if let Ok(n) = s.parse::<f32>() {
        if n < 10.0 {
            return Some((n * 1000.0) as u16);
        } else {
            return Some(n as u16);
        }
    }

    None
}

/// Parameters describing a bulk read operation for protocol V2/V3 command packets.
///
/// Higher-level code provides this to influence the read mode, opcode, dummy cycles,
/// and 4-byte address handling in the command packet sent to the dediprog firmware.
#[derive(Debug, Clone, Copy)]
pub struct BulkReadOp {
    /// SPI read opcode (e.g., 0x03 for READ, 0x0B for FAST_READ, 0x3B for DOR, etc.)
    pub opcode: u8,
    /// Whether this opcode natively uses 4-byte addresses (e.g., 0x13, 0x0C)
    pub native_4ba: bool,
    /// Number of dummy cycles required by this read command
    pub dummy_cycles: u8,
    /// I/O mode to use for this read (maps to CMD_IO_MODE)
    pub io_mode: DpIoMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_voltage() {
        assert_eq!(parse_voltage("3.5V"), Some(3500));
        assert_eq!(parse_voltage("3.5v"), Some(3500));
        assert_eq!(parse_voltage("3.5"), Some(3500));
        assert_eq!(parse_voltage("1800mV"), Some(1800));
        assert_eq!(parse_voltage("1800mv"), Some(1800));
        assert_eq!(parse_voltage("1.8V"), Some(1800));
        assert_eq!(parse_voltage("1.8"), Some(1800));
        assert_eq!(parse_voltage("2.5"), Some(2500));
    }

    #[test]
    fn test_device_type_from_string() {
        assert_eq!(
            DeviceType::from_device_string("SF100 V:5.0.0"),
            DeviceType::SF100
        );
        assert_eq!(
            DeviceType::from_device_string("SF600PG2 V:1.0.0"),
            DeviceType::SF600PG2
        );
        assert_eq!(
            DeviceType::from_device_string("SF600 V:7.0.0"),
            DeviceType::SF600
        );
        assert_eq!(
            DeviceType::from_device_string("SF700 V:4.0.0"),
            DeviceType::SF700
        );
    }

    #[test]
    fn test_protocol_version() {
        // SF100 with old firmware -> V1
        assert_eq!(
            Protocol::from_device_firmware(DeviceType::SF100, firmware_version(5, 0, 0)),
            Protocol::V1
        );
        // SF100 with new firmware -> V2
        assert_eq!(
            Protocol::from_device_firmware(DeviceType::SF100, firmware_version(5, 5, 0)),
            Protocol::V2
        );
        // SF600 with various firmwares
        assert_eq!(
            Protocol::from_device_firmware(DeviceType::SF600, firmware_version(6, 8, 0)),
            Protocol::V1
        );
        assert_eq!(
            Protocol::from_device_firmware(DeviceType::SF600, firmware_version(7, 2, 21)),
            Protocol::V2
        );
        assert_eq!(
            Protocol::from_device_firmware(DeviceType::SF600, firmware_version(7, 2, 22)),
            Protocol::V3
        );
        // SF700 always V3
        assert_eq!(
            Protocol::from_device_firmware(DeviceType::SF700, firmware_version(4, 0, 0)),
            Protocol::V3
        );
    }
}
