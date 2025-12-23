//! FTDI MPSSE protocol constants
//!
//! Based on flashprog/ft2232_spi.c and FTDI MPSSE documentation.

// Allow unused constants - they're provided for completeness
#![allow(dead_code)]

// ============================================================================
// USB VID/PID constants
// ============================================================================

/// FTDI vendor ID
pub const FTDI_VID: u16 = 0x0403;

/// FT2232H product ID (dual channel)
pub const FTDI_FT2232H_PID: u16 = 0x6010;

/// FT4232H product ID (quad channel)
pub const FTDI_FT4232H_PID: u16 = 0x6011;

/// FT232H product ID (single channel)
pub const FTDI_FT232H_PID: u16 = 0x6014;

/// FT4233H product ID (quad channel)
pub const FTDI_FT4233H_PID: u16 = 0x6041;

/// TIAO TUMPA product ID
pub const TIAO_TUMPA_PID: u16 = 0x8A98;

/// TIAO TUMPA Lite product ID
pub const TIAO_TUMPA_LITE_PID: u16 = 0x8A99;

/// Kristech KT-LINK product ID
pub const KT_LINK_PID: u16 = 0xBBE2;

/// Amontec JTAGkey product ID
pub const AMONTEC_JTAGKEY_PID: u16 = 0xCFF8;

/// GOEPEL vendor ID
pub const GOEPEL_VID: u16 = 0x096C;

/// GOEPEL PicoTAP product ID
pub const GOEPEL_PICOTAP_PID: u16 = 0x1449;

/// FIC vendor ID
pub const FIC_VID: u16 = 0x1457;

/// OpenMoko Neo1973 Debug board product ID
pub const OPENMOKO_DBGBOARD_PID: u16 = 0x5118;

/// Olimex vendor ID
pub const OLIMEX_VID: u16 = 0x15BA;

/// Olimex ARM-USB-OCD product ID
pub const OLIMEX_ARM_OCD_PID: u16 = 0x0003;

/// Olimex ARM-USB-TINY product ID
pub const OLIMEX_ARM_TINY_PID: u16 = 0x0004;

/// Olimex ARM-USB-OCD-H product ID
pub const OLIMEX_ARM_OCD_H_PID: u16 = 0x002B;

/// Olimex ARM-USB-TINY-H product ID
pub const OLIMEX_ARM_TINY_H_PID: u16 = 0x002A;

/// Google vendor ID
pub const GOOGLE_VID: u16 = 0x18D1;

/// Google Servo product ID
pub const GOOGLE_SERVO_PID: u16 = 0x5001;

/// Google Servo V2 Legacy product ID
pub const GOOGLE_SERVO_V2_PID0: u16 = 0x5002;

/// Google Servo V2 product ID
pub const GOOGLE_SERVO_V2_PID1: u16 = 0x5003;

// ============================================================================
// MPSSE Commands
// ============================================================================

/// Write bytes on negative clock edge (SPI mode 0/2)
pub const MPSSE_DO_WRITE: u8 = 0x10;

/// Read bytes on positive clock edge (SPI mode 0/2)
pub const MPSSE_DO_READ: u8 = 0x20;

/// Write on negative clock edge
pub const MPSSE_WRITE_NEG: u8 = 0x01;

/// Read on negative clock edge
pub const MPSSE_READ_NEG: u8 = 0x04;

/// LSB first (not used - SPI is MSB first)
pub const MPSSE_LSB: u8 = 0x08;

/// Bit mode (transfer bits instead of bytes)
pub const MPSSE_BITMODE: u8 = 0x02;

/// Set data bits low byte
pub const SET_BITS_LOW: u8 = 0x80;

/// Set data bits high byte
pub const SET_BITS_HIGH: u8 = 0x82;

/// Get data bits low byte
pub const GET_BITS_LOW: u8 = 0x81;

/// Get data bits high byte
pub const GET_BITS_HIGH: u8 = 0x83;

/// Disable loopback mode
pub const LOOPBACK_END: u8 = 0x85;

/// Set clock divisor
pub const TCK_DIVISOR: u8 = 0x86;

/// Disable divide-by-5 prescaler (60 MHz clock)
pub const DIS_DIV_5: u8 = 0x8A;

/// Enable divide-by-5 prescaler (12 MHz clock)
pub const EN_DIV_5: u8 = 0x8B;

/// Enable 3-phase clocking (for I2C)
pub const EN_3_PHASE: u8 = 0x8C;

/// Disable 3-phase clocking
pub const DIS_3_PHASE: u8 = 0x8D;

/// Enable adaptive clocking
pub const CLK_ADAPTIVE: u8 = 0x96;

/// Disable adaptive clocking
pub const CLK_NO_ADAPTIVE: u8 = 0x97;

/// Send immediate (flush buffers)
pub const SEND_IMMEDIATE: u8 = 0x87;

/// Wait on I/O high
pub const WAIT_ON_HIGH: u8 = 0x88;

/// Wait on I/O low
pub const WAIT_ON_LOW: u8 = 0x89;

// ============================================================================
// Buffer sizes
// ============================================================================

/// FTDI hardware buffer size in bytes
pub const FTDI_HW_BUFFER_SIZE: usize = 4096;

/// Default clock divisor (15 MHz at 60 MHz base clock)
pub const DEFAULT_DIVISOR: u16 = 2;

// ============================================================================
// Pin assignments (low byte)
//
// TCK/SK is bit 0.  (clock)
// TDI/DO is bit 1.  (data out)
// TDO/DI is bit 2.  (data in)
// TMS/CS is bit 3.  (chip select)
// GPIOL0 is bit 4.
// GPIOL1 is bit 5.
// GPIOL2 is bit 6.
// GPIOL3 is bit 7.
// ============================================================================

/// Bit position for SK (clock)
pub const PIN_SK: u8 = 0;

/// Bit position for DO (data out / MOSI)
pub const PIN_DO: u8 = 1;

/// Bit position for DI (data in / MISO)
pub const PIN_DI: u8 = 2;

/// Bit position for CS (chip select)
pub const PIN_CS: u8 = 3;

/// Bit position for GPIOL0
pub const PIN_GPIOL0: u8 = 4;

/// Bit position for GPIOL1
pub const PIN_GPIOL1: u8 = 5;

/// Bit position for GPIOL2
pub const PIN_GPIOL2: u8 = 6;

/// Bit position for GPIOL3
pub const PIN_GPIOL3: u8 = 7;

/// Default CS bits (CS high)
pub const DEFAULT_CS_BITS: u8 = 1 << PIN_CS;

/// Default pin direction (SK, DO, CS as outputs)
pub const DEFAULT_PINDIR: u8 = (1 << PIN_SK) | (1 << PIN_DO) | (1 << PIN_CS);

// ============================================================================
// Supported device types
// ============================================================================

/// Supported FTDI device types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FtdiDeviceType {
    /// FT2232H (dual channel, 60 MHz)
    Ft2232H,
    /// FT4232H (quad channel, 60 MHz)
    #[default]
    Ft4232H,
    /// FT232H (single channel, 60 MHz)
    Ft232H,
    /// FT4233H (quad channel, 60 MHz)
    Ft4233H,
    /// TIAO TUMPA
    Tumpa,
    /// TIAO TUMPA Lite
    TumpaLite,
    /// Kristech KT-LINK
    KtLink,
    /// Amontec JTAGkey
    JtagKey,
    /// GOEPEL PicoTAP
    PicoTap,
    /// Olimex ARM-USB-OCD
    ArmUsbOcd,
    /// Olimex ARM-USB-TINY
    ArmUsbTiny,
    /// Olimex ARM-USB-OCD-H
    ArmUsbOcdH,
    /// Olimex ARM-USB-TINY-H
    ArmUsbTinyH,
    /// Google Servo
    GoogleServo,
    /// Google Servo V2
    GoogleServoV2,
    /// Google Servo V2 Legacy
    GoogleServoV2Legacy,
    /// OpenMoko Debug Board
    OpenMokoDbg,
    /// Bus Blaster
    BusBlaster,
    /// Flyswatter
    Flyswatter,
}

impl FtdiDeviceType {
    /// Get the vendor ID for this device type
    pub fn vendor_id(&self) -> u16 {
        match self {
            FtdiDeviceType::PicoTap => GOEPEL_VID,
            FtdiDeviceType::OpenMokoDbg => FIC_VID,
            FtdiDeviceType::ArmUsbOcd
            | FtdiDeviceType::ArmUsbTiny
            | FtdiDeviceType::ArmUsbOcdH
            | FtdiDeviceType::ArmUsbTinyH => OLIMEX_VID,
            FtdiDeviceType::GoogleServo
            | FtdiDeviceType::GoogleServoV2
            | FtdiDeviceType::GoogleServoV2Legacy => GOOGLE_VID,
            _ => FTDI_VID,
        }
    }

    /// Get the product ID for this device type
    pub fn product_id(&self) -> u16 {
        match self {
            FtdiDeviceType::Ft2232H | FtdiDeviceType::BusBlaster | FtdiDeviceType::Flyswatter => {
                FTDI_FT2232H_PID
            }
            FtdiDeviceType::Ft4232H => FTDI_FT4232H_PID,
            FtdiDeviceType::Ft232H => FTDI_FT232H_PID,
            FtdiDeviceType::Ft4233H => FTDI_FT4233H_PID,
            FtdiDeviceType::Tumpa => TIAO_TUMPA_PID,
            FtdiDeviceType::TumpaLite => TIAO_TUMPA_LITE_PID,
            FtdiDeviceType::KtLink => KT_LINK_PID,
            FtdiDeviceType::JtagKey => AMONTEC_JTAGKEY_PID,
            FtdiDeviceType::PicoTap => GOEPEL_PICOTAP_PID,
            FtdiDeviceType::OpenMokoDbg => OPENMOKO_DBGBOARD_PID,
            FtdiDeviceType::ArmUsbOcd => OLIMEX_ARM_OCD_PID,
            FtdiDeviceType::ArmUsbTiny => OLIMEX_ARM_TINY_PID,
            FtdiDeviceType::ArmUsbOcdH => OLIMEX_ARM_OCD_H_PID,
            FtdiDeviceType::ArmUsbTinyH => OLIMEX_ARM_TINY_H_PID,
            FtdiDeviceType::GoogleServo => GOOGLE_SERVO_PID,
            FtdiDeviceType::GoogleServoV2 => GOOGLE_SERVO_V2_PID1,
            FtdiDeviceType::GoogleServoV2Legacy => GOOGLE_SERVO_V2_PID0,
        }
    }

    /// Get the number of channels for this device type
    pub fn channel_count(&self) -> u8 {
        match self {
            FtdiDeviceType::Ft232H | FtdiDeviceType::TumpaLite | FtdiDeviceType::KtLink => 1,
            FtdiDeviceType::Ft2232H
            | FtdiDeviceType::Tumpa
            | FtdiDeviceType::JtagKey
            | FtdiDeviceType::PicoTap
            | FtdiDeviceType::OpenMokoDbg
            | FtdiDeviceType::ArmUsbOcd
            | FtdiDeviceType::ArmUsbTiny
            | FtdiDeviceType::ArmUsbOcdH
            | FtdiDeviceType::ArmUsbTinyH
            | FtdiDeviceType::BusBlaster
            | FtdiDeviceType::Flyswatter => 2,
            FtdiDeviceType::Ft4232H
            | FtdiDeviceType::Ft4233H
            | FtdiDeviceType::GoogleServo
            | FtdiDeviceType::GoogleServoV2
            | FtdiDeviceType::GoogleServoV2Legacy => 4,
        }
    }

    /// Get the default CS bits for this device type
    pub fn default_cs_bits(&self) -> u8 {
        match self {
            // JTAGkey needs OE (GPIOL0) high + CS high
            FtdiDeviceType::JtagKey | FtdiDeviceType::BusBlaster => 0x18,
            // ARM-USB-OCD(-H) has output buffer needing ADBUS4 low
            FtdiDeviceType::ArmUsbOcd | FtdiDeviceType::ArmUsbOcdH => 0x08,
            _ => DEFAULT_CS_BITS,
        }
    }

    /// Get the default pin direction for this device type
    pub fn default_pindir(&self) -> u8 {
        match self {
            // JTAGkey: OE=output, CS=output, DI=input, DO=output, SK=output
            FtdiDeviceType::JtagKey | FtdiDeviceType::BusBlaster => 0x1B,
            // ARM-USB-OCD(-H): #OE=output, CS=output, DI=input, DO=output, SK=output
            FtdiDeviceType::ArmUsbOcd | FtdiDeviceType::ArmUsbOcdH => 0x1B,
            // KT-LINK: GPIOL1 output for TMS/TDO mux
            FtdiDeviceType::KtLink => 0x2B,
            // Flyswatter: GPIO 6,7 low to enable output buffers
            FtdiDeviceType::Flyswatter => 0xCB,
            _ => DEFAULT_PINDIR,
        }
    }

    /// Get the default auxiliary bits for this device type
    pub fn default_aux_bits(&self) -> u8 {
        match self {
            // KT-LINK: set GPIOL1 high
            FtdiDeviceType::KtLink => 0x20,
            _ => 0x00,
        }
    }

    /// Get the default high-byte pin direction
    pub fn default_pindir_high(&self) -> u8 {
        match self {
            // KT-LINK: GPIOH4,5,6 as outputs (buffer enables)
            FtdiDeviceType::KtLink => 0x70,
            _ => 0x00,
        }
    }

    /// Get the default clock divisor for this device type
    pub fn default_divisor(&self) -> u16 {
        match self {
            // Google Servo V2 needs slower clock
            FtdiDeviceType::GoogleServoV2 => 6,
            _ => DEFAULT_DIVISOR,
        }
    }

    /// Whether this device supports 60 MHz base clock
    pub fn is_high_speed(&self) -> bool {
        // All supported devices in this enum are high-speed 'H' variants
        true
    }

    /// Parse device type from string
    pub fn parse(s: &str) -> Option<Self> {
        let s_lower = s.to_lowercase();
        match s_lower.as_str() {
            "2232h" | "ft2232h" => Some(FtdiDeviceType::Ft2232H),
            "4232h" | "ft4232h" => Some(FtdiDeviceType::Ft4232H),
            "232h" | "ft232h" => Some(FtdiDeviceType::Ft232H),
            "4233h" | "ft4233h" => Some(FtdiDeviceType::Ft4233H),
            "tumpa" => Some(FtdiDeviceType::Tumpa),
            "tumpalite" => Some(FtdiDeviceType::TumpaLite),
            "kt-link" | "ktlink" => Some(FtdiDeviceType::KtLink),
            "jtagkey" => Some(FtdiDeviceType::JtagKey),
            "picotap" => Some(FtdiDeviceType::PicoTap),
            "openmoko" => Some(FtdiDeviceType::OpenMokoDbg),
            "arm-usb-ocd" => Some(FtdiDeviceType::ArmUsbOcd),
            "arm-usb-tiny" => Some(FtdiDeviceType::ArmUsbTiny),
            "arm-usb-ocd-h" => Some(FtdiDeviceType::ArmUsbOcdH),
            "arm-usb-tiny-h" => Some(FtdiDeviceType::ArmUsbTinyH),
            "google-servo" => Some(FtdiDeviceType::GoogleServo),
            "google-servo-v2" => Some(FtdiDeviceType::GoogleServoV2),
            "google-servo-v2-legacy" => Some(FtdiDeviceType::GoogleServoV2Legacy),
            "busblaster" => Some(FtdiDeviceType::BusBlaster),
            "flyswatter" => Some(FtdiDeviceType::Flyswatter),
            _ => None,
        }
    }

    /// Get the name of this device type
    pub fn name(&self) -> &'static str {
        match self {
            FtdiDeviceType::Ft2232H => "FT2232H",
            FtdiDeviceType::Ft4232H => "FT4232H",
            FtdiDeviceType::Ft232H => "FT232H",
            FtdiDeviceType::Ft4233H => "FT4233H",
            FtdiDeviceType::Tumpa => "TUMPA",
            FtdiDeviceType::TumpaLite => "TUMPA Lite",
            FtdiDeviceType::KtLink => "KT-LINK",
            FtdiDeviceType::JtagKey => "JTAGkey",
            FtdiDeviceType::PicoTap => "PicoTAP",
            FtdiDeviceType::OpenMokoDbg => "OpenMoko Debug Board",
            FtdiDeviceType::ArmUsbOcd => "ARM-USB-OCD",
            FtdiDeviceType::ArmUsbTiny => "ARM-USB-TINY",
            FtdiDeviceType::ArmUsbOcdH => "ARM-USB-OCD-H",
            FtdiDeviceType::ArmUsbTinyH => "ARM-USB-TINY-H",
            FtdiDeviceType::GoogleServo => "Google Servo",
            FtdiDeviceType::GoogleServoV2 => "Google Servo V2",
            FtdiDeviceType::GoogleServoV2Legacy => "Google Servo V2 Legacy",
            FtdiDeviceType::BusBlaster => "Bus Blaster",
            FtdiDeviceType::Flyswatter => "Flyswatter",
        }
    }
}

/// FTDI interface/channel selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FtdiInterface {
    /// Channel A (default)
    #[default]
    A,
    /// Channel B
    B,
    /// Channel C
    C,
    /// Channel D
    D,
}

impl FtdiInterface {
    /// Parse interface from character
    pub fn from_char(c: char) -> Option<Self> {
        match c.to_ascii_uppercase() {
            'A' => Some(FtdiInterface::A),
            'B' => Some(FtdiInterface::B),
            'C' => Some(FtdiInterface::C),
            'D' => Some(FtdiInterface::D),
            _ => None,
        }
    }

    /// Get the interface index (0-3)
    pub fn index(&self) -> u8 {
        match self {
            FtdiInterface::A => 0,
            FtdiInterface::B => 1,
            FtdiInterface::C => 2,
            FtdiInterface::D => 3,
        }
    }

    /// Get the channel letter
    pub fn letter(&self) -> char {
        match self {
            FtdiInterface::A => 'A',
            FtdiInterface::B => 'B',
            FtdiInterface::C => 'C',
            FtdiInterface::D => 'D',
        }
    }
}

/// Supported FTDI devices for enumeration
pub struct SupportedDevice {
    pub vendor_id: u16,
    pub product_id: u16,
    pub vendor_name: &'static str,
    pub device_name: &'static str,
}

/// List of all supported FTDI devices
pub const SUPPORTED_DEVICES: &[SupportedDevice] = &[
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: FTDI_FT2232H_PID,
        vendor_name: "FTDI",
        device_name: "FT2232H",
    },
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: FTDI_FT4232H_PID,
        vendor_name: "FTDI",
        device_name: "FT4232H",
    },
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: FTDI_FT232H_PID,
        vendor_name: "FTDI",
        device_name: "FT232H",
    },
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: FTDI_FT4233H_PID,
        vendor_name: "FTDI",
        device_name: "FT4233H",
    },
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: TIAO_TUMPA_PID,
        vendor_name: "TIAO",
        device_name: "USB Multi-Protocol Adapter",
    },
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: TIAO_TUMPA_LITE_PID,
        vendor_name: "TIAO",
        device_name: "USB Multi-Protocol Adapter Lite",
    },
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: KT_LINK_PID,
        vendor_name: "Kristech",
        device_name: "KT-LINK",
    },
    SupportedDevice {
        vendor_id: FTDI_VID,
        product_id: AMONTEC_JTAGKEY_PID,
        vendor_name: "Amontec",
        device_name: "JTAGkey",
    },
    SupportedDevice {
        vendor_id: GOEPEL_VID,
        product_id: GOEPEL_PICOTAP_PID,
        vendor_name: "GOEPEL",
        device_name: "PicoTAP",
    },
    SupportedDevice {
        vendor_id: GOOGLE_VID,
        product_id: GOOGLE_SERVO_PID,
        vendor_name: "Google",
        device_name: "Servo",
    },
    SupportedDevice {
        vendor_id: GOOGLE_VID,
        product_id: GOOGLE_SERVO_V2_PID0,
        vendor_name: "Google",
        device_name: "Servo V2 Legacy",
    },
    SupportedDevice {
        vendor_id: GOOGLE_VID,
        product_id: GOOGLE_SERVO_V2_PID1,
        vendor_name: "Google",
        device_name: "Servo V2",
    },
    SupportedDevice {
        vendor_id: FIC_VID,
        product_id: OPENMOKO_DBGBOARD_PID,
        vendor_name: "FIC",
        device_name: "OpenMoko Neo1973 Debug board (V2+)",
    },
    SupportedDevice {
        vendor_id: OLIMEX_VID,
        product_id: OLIMEX_ARM_OCD_PID,
        vendor_name: "Olimex",
        device_name: "ARM-USB-OCD",
    },
    SupportedDevice {
        vendor_id: OLIMEX_VID,
        product_id: OLIMEX_ARM_TINY_PID,
        vendor_name: "Olimex",
        device_name: "ARM-USB-TINY",
    },
    SupportedDevice {
        vendor_id: OLIMEX_VID,
        product_id: OLIMEX_ARM_OCD_H_PID,
        vendor_name: "Olimex",
        device_name: "ARM-USB-OCD-H",
    },
    SupportedDevice {
        vendor_id: OLIMEX_VID,
        product_id: OLIMEX_ARM_TINY_H_PID,
        vendor_name: "Olimex",
        device_name: "ARM-USB-TINY-H",
    },
];

/// Check if a VID/PID pair matches a supported FTDI device
pub fn is_supported_device(vid: u16, pid: u16) -> bool {
    SUPPORTED_DEVICES
        .iter()
        .any(|d| d.vendor_id == vid && d.product_id == pid)
}

/// Get device info for a VID/PID pair
pub fn get_device_info(vid: u16, pid: u16) -> Option<&'static SupportedDevice> {
    SUPPORTED_DEVICES
        .iter()
        .find(|d| d.vendor_id == vid && d.product_id == pid)
}
