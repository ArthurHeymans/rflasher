//! Transport layer abstraction for serprog communication
//!
//! This module provides a unified interface for serial and TCP transports.

use crate::error::{Result, SerprogError};

/// Transport trait for reading and writing bytes
pub trait Transport {
    /// Write bytes to the transport
    fn write(&mut self, data: &[u8]) -> Result<()>;

    /// Read bytes from the transport
    ///
    /// Reads exactly `buf.len()` bytes into the buffer.
    /// Returns an error if not enough bytes are available.
    fn read(&mut self, buf: &mut [u8]) -> Result<()>;

    /// Read with timeout (non-blocking)
    ///
    /// Reads up to `buf.len()` bytes, waiting up to `timeout_ms` milliseconds.
    /// Returns the number of bytes read, or 0 if timeout.
    fn read_nonblock(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize>;

    /// Write with timeout (non-blocking)
    ///
    /// Returns true if write succeeded, false on timeout.
    fn write_nonblock(&mut self, data: &[u8], timeout_ms: u32) -> Result<bool>;

    /// Flush any buffered data
    fn flush(&mut self) -> Result<()>;
}

#[cfg(feature = "std")]
pub mod serial {
    //! Serial port transport implementation

    use super::*;
    use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};
    use std::io::{Read, Write};
    use std::time::Duration;

    /// Serial port transport
    pub struct SerialTransport {
        port: Box<dyn SerialPort>,
    }

    impl SerialTransport {
        /// Open a serial port with the specified baud rate
        ///
        /// If baud is 0 or negative, uses the hardware default (typically 115200).
        pub fn open(device: &str, baud: Option<u32>) -> Result<Self> {
            let baud_rate = baud.unwrap_or(115200);

            let port = serialport::new(device, baud_rate)
                .data_bits(DataBits::Eight)
                .parity(Parity::None)
                .stop_bits(StopBits::One)
                .flow_control(FlowControl::None)
                .timeout(Duration::from_secs(5))
                .open()?;

            log::info!("Opened serial port {} at {} baud", device, baud_rate);

            Ok(Self { port })
        }

        /// Set the read timeout
        pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
            self.port.set_timeout(timeout)?;
            Ok(())
        }
    }

    impl Transport for SerialTransport {
        fn write(&mut self, data: &[u8]) -> Result<()> {
            self.port.write_all(data)?;
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> Result<()> {
            self.port.read_exact(buf)?;
            Ok(())
        }

        fn read_nonblock(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize> {
            // Set temporary timeout
            let old_timeout = self.port.timeout();
            self.port
                .set_timeout(Duration::from_millis(timeout_ms as u64))?;

            let result = match self.port.read(buf) {
                Ok(n) => Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
                Err(e) => Err(SerprogError::from(e)),
            };

            // Restore timeout
            self.port.set_timeout(old_timeout)?;
            result
        }

        fn write_nonblock(&mut self, data: &[u8], timeout_ms: u32) -> Result<bool> {
            // Set temporary timeout
            let old_timeout = self.port.timeout();
            self.port
                .set_timeout(Duration::from_millis(timeout_ms as u64))?;

            let result = match self.port.write_all(data) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(false),
                Err(e) => Err(SerprogError::from(e)),
            };

            // Restore timeout
            self.port.set_timeout(old_timeout)?;
            result
        }

        fn flush(&mut self) -> Result<()> {
            self.port.flush()?;
            Ok(())
        }
    }
}

#[cfg(feature = "std")]
pub mod tcp {
    //! TCP socket transport implementation

    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    /// TCP socket transport
    pub struct TcpTransport {
        stream: TcpStream,
    }

    impl TcpTransport {
        /// Connect to a serprog server at the specified host and port
        pub fn connect(host: &str, port: u16) -> Result<Self> {
            let addr = format!("{}:{}", host, port);
            log::info!("Connecting to serprog server at {}", addr);

            let stream = TcpStream::connect(&addr)
                .map_err(|e| SerprogError::ConnectionFailed(e.to_string()))?;

            // Set TCP_NODELAY to reduce latency
            stream.set_nodelay(true).map_err(|e| {
                SerprogError::ConnectionFailed(format!("Failed to set TCP_NODELAY: {}", e))
            })?;

            // Set default timeout
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| {
                    SerprogError::ConnectionFailed(format!("Failed to set read timeout: {}", e))
                })?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| {
                    SerprogError::ConnectionFailed(format!("Failed to set write timeout: {}", e))
                })?;

            log::info!("Connected to serprog server at {}", addr);

            Ok(Self { stream })
        }
    }

    impl Transport for TcpTransport {
        fn write(&mut self, data: &[u8]) -> Result<()> {
            self.stream.write_all(data)?;
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> Result<()> {
            self.stream.read_exact(buf)?;
            Ok(())
        }

        fn read_nonblock(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize> {
            // Set temporary timeout
            self.stream
                .set_read_timeout(Some(Duration::from_millis(timeout_ms as u64)))?;

            let result = match self.stream.read(buf) {
                Ok(n) => Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
                Err(e) => Err(SerprogError::from(e)),
            };

            // Restore default timeout
            self.stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            result
        }

        fn write_nonblock(&mut self, data: &[u8], timeout_ms: u32) -> Result<bool> {
            // Set temporary timeout
            self.stream
                .set_write_timeout(Some(Duration::from_millis(timeout_ms as u64)))?;

            let result = match self.stream.write_all(data) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(false),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
                Err(e) => Err(SerprogError::from(e)),
            };

            // Restore default timeout
            self.stream
                .set_write_timeout(Some(Duration::from_secs(5)))?;
            result
        }

        fn flush(&mut self) -> Result<()> {
            self.stream.flush()?;
            Ok(())
        }
    }
}
