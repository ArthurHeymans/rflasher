//! rflasher-serprog - Serial Flasher Protocol support
//!
//! This crate implements the serprog protocol for communication with
//! microcontroller-based flash programmers.
//!
//! # Protocol Overview
//!
//! The Serial Flasher Protocol (serprog) is a simple protocol for communicating
//! with flash programmers over serial ports or TCP sockets. It supports various
//! commands for SPI operations, bus type selection, and programmer configuration.
//!
//! # Supported Transports
//!
//! - Serial port: `/dev/ttyUSB0`, `/dev/ttyACM0`, `COM1`, etc.
//! - TCP socket: `host:port`
//!
//! # Example
//!
//! ```no_run
//! use rflasher_serprog::{Serprog, SerialTransport};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! // Open a serial connection
//! let transport = SerialTransport::open("/dev/ttyUSB0", Some(115200))?;
//! let mut serprog = Serprog::new(transport)?;
//!
//! // Optionally set SPI speed
//! serprog.set_spi_speed(2_000_000)?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! serprog.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod error;
pub mod protocol;

#[cfg(feature = "std")]
pub mod device;
#[cfg(feature = "std")]
pub mod transport;

// Re-exports
pub use error::{Result, SerprogError};
pub use protocol::{bus, CommandMap, ProgrammerInfo};

#[cfg(feature = "std")]
pub use device::Serprog;
#[cfg(feature = "std")]
pub use transport::serial::SerialTransport;
#[cfg(feature = "std")]
pub use transport::tcp::TcpTransport;
#[cfg(feature = "std")]
pub use transport::Transport;

/// Connection options for serprog
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub enum SerprogConnection {
    /// Serial port connection
    Serial {
        /// Device path (e.g., "/dev/ttyUSB0" or "COM1")
        device: String,
        /// Baud rate (None for hardware default)
        baud: Option<u32>,
    },
    /// TCP socket connection
    Tcp {
        /// Hostname or IP address
        host: String,
        /// Port number
        port: u16,
    },
}

#[cfg(feature = "std")]
impl SerprogConnection {
    /// Parse a connection string
    ///
    /// Formats:
    /// - `dev=/dev/ttyUSB0` - Serial with default baud
    /// - `dev=/dev/ttyUSB0:115200` - Serial with specified baud
    /// - `ip=host:port` - TCP connection
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        if let Some(dev) = s.strip_prefix("dev=") {
            // Serial connection
            if let Some((device, baud_str)) = dev.rsplit_once(':') {
                let baud = baud_str
                    .parse()
                    .map_err(|_| format!("Invalid baud rate: {}", baud_str))?;
                Ok(SerprogConnection::Serial {
                    device: device.to_string(),
                    baud: Some(baud),
                })
            } else {
                Ok(SerprogConnection::Serial {
                    device: dev.to_string(),
                    baud: None,
                })
            }
        } else if let Some(ip) = s.strip_prefix("ip=") {
            // TCP connection
            let (host, port_str) = ip
                .rsplit_once(':')
                .ok_or_else(|| "Missing port in ip= parameter".to_string())?;
            let port = port_str
                .parse()
                .map_err(|_| format!("Invalid port: {}", port_str))?;
            Ok(SerprogConnection::Tcp {
                host: host.to_string(),
                port,
            })
        } else {
            Err(format!(
                "Invalid serprog connection string: {}. Use dev=... or ip=...",
                s
            ))
        }
    }
}

/// Open a serprog connection and return a boxed SpiMaster
///
/// This is a convenience function that handles both serial and TCP connections
/// and returns a type-erased SpiMaster.
#[cfg(feature = "std")]
pub fn open_serprog(
    options: &str,
) -> std::result::Result<Box<dyn rflasher_core::programmer::SpiMaster>, Box<dyn std::error::Error>>
{
    let conn = SerprogConnection::parse(options)?;

    match conn {
        SerprogConnection::Serial { device, baud } => {
            let transport = SerialTransport::open(&device, baud)?;
            let serprog = Serprog::new(transport)?;
            Ok(Box::new(serprog))
        }
        SerprogConnection::Tcp { host, port } => {
            let transport = TcpTransport::connect(&host, port)?;
            let serprog = Serprog::new(transport)?;
            Ok(Box::new(serprog))
        }
    }
}

/// Open a serprog connection via serial port
#[cfg(feature = "std")]
pub fn open_serial(device: &str, baud: Option<u32>) -> Result<Serprog<SerialTransport>> {
    let transport = SerialTransport::open(device, baud)?;
    Serprog::new(transport)
}

/// Open a serprog connection via TCP
#[cfg(feature = "std")]
pub fn open_tcp(host: &str, port: u16) -> Result<Serprog<TcpTransport>> {
    let transport = TcpTransport::connect(host, port)?;
    Serprog::new(transport)
}
