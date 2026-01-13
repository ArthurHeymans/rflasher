//! WebSerial transport implementation for browser-based serprog communication
//!
//! This module provides a WebSerial-based transport that implements the
//! `Transport` trait from rflasher-serprog for async mode.
//!
//! Uses web-sys bindings for the WebSerial API types.

// Allow deprecated JsStatic - single-threaded WASM doesn't need thread_local_v2
#![allow(deprecated)]

use js_sys::Reflect;
use maybe_async::maybe_async;
use rflasher_serprog::error::{Result, SerprogError};
use rflasher_serprog::Transport;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FlowControlType, ReadableStreamDefaultReader, SerialOptions, SerialPort,
    WritableStreamDefaultWriter,
};

// Minimal binding to access navigator.serial (not directly exposed in web-sys Navigator)
#[wasm_bindgen]
extern "C" {
    /// Navigator.serial - the Serial interface from the WebSerial API
    #[wasm_bindgen(js_namespace = navigator, js_name = serial)]
    pub static SERIAL: web_sys::Serial;
}

/// WebSerial transport for browser-based serprog communication
pub struct WebSerialTransport {
    port: SerialPort,
    reader: ReadableStreamDefaultReader,
    writer: WritableStreamDefaultWriter,
    /// Buffer for excess bytes read from the stream
    read_buffer: Vec<u8>,
}

impl WebSerialTransport {
    /// Request a serial port from the user and open it
    ///
    /// This will show a browser dialog for the user to select a serial device.
    pub async fn request_and_open(baud_rate: u32) -> Result<Self> {
        // Request a port from the user using web-sys Serial
        let port_promise = SERIAL.request_port();
        let port: SerialPort = JsFuture::from(port_promise)
            .await
            .map_err(|e| {
                SerprogError::ConnectionFailed(format!("Failed to request port: {:?}", e))
            })?
            .unchecked_into();

        Self::open_port(port, baud_rate).await
    }

    /// Open an already-selected serial port
    pub async fn open_port(port: SerialPort, baud_rate: u32) -> Result<Self> {
        // Configure port options using web-sys SerialOptions
        let options = SerialOptions::new(baud_rate);

        // Set a larger buffer size (default is 255 bytes, we want more for bulk transfers)
        options.set_buffer_size(64 * 1024);

        // Use hardware flow control if the device supports it
        options.set_flow_control(FlowControlType::Hardware);

        log::info!(
            "Opening port with baudRate={}, bufferSize=64KB, flowControl=hardware",
            baud_rate
        );

        // Open the port - SerialPort::open returns a Promise
        let open_promise = port.open(&options);
        JsFuture::from(open_promise)
            .await
            .map_err(|e| SerprogError::ConnectionFailed(format!("Failed to open port: {:?}", e)))?;

        // Get reader from readable stream
        let readable = port.readable();
        let reader: ReadableStreamDefaultReader = readable.get_reader().unchecked_into();

        // Get writer from writable stream
        let writable = port.writable();
        let writer: WritableStreamDefaultWriter = writable
            .get_writer()
            .map_err(|e| SerprogError::ConnectionFailed(format!("Failed to get writer: {:?}", e)))?
            .unchecked_into();

        log::info!("WebSerial port opened at {} baud", baud_rate);

        Ok(Self {
            port,
            reader,
            writer,
            read_buffer: Vec::new(),
        })
    }

    /// Close the port
    #[allow(dead_code)]
    pub async fn close(&self) -> Result<()> {
        // Release the reader and writer first
        self.reader.release_lock();
        self.writer.release_lock();

        // Close the port - returns a Promise
        let close_promise = self.port.close();
        JsFuture::from(close_promise).await.map_err(|e| {
            SerprogError::ConnectionFailed(format!("Failed to close port: {:?}", e))
        })?;

        log::info!("WebSerial port closed");
        Ok(())
    }

    /// Read a chunk from the stream
    async fn read_chunk(&mut self) -> Result<Vec<u8>> {
        // ReadableStreamDefaultReader::read returns a Promise
        let read_promise = self.reader.read();
        let result = JsFuture::from(read_promise)
            .await
            .map_err(|e| SerprogError::IoError(format!("Read failed: {:?}", e)))?;

        // Check if stream is done
        let done = Reflect::get(&result, &JsValue::from_str("done"))
            .map_err(|_| SerprogError::IoError("Failed to get done flag".to_string()))?
            .as_bool()
            .unwrap_or(false);

        if done {
            return Err(SerprogError::IoError("Stream ended".to_string()));
        }

        // Get the value (Uint8Array)
        let value = Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|_| SerprogError::IoError("Failed to get value".to_string()))?;

        let array: js_sys::Uint8Array = value
            .dyn_into()
            .map_err(|_| SerprogError::IoError("Value is not Uint8Array".to_string()))?;

        Ok(array.to_vec())
    }
}

#[maybe_async(AFIT)]
impl Transport for WebSerialTransport {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        log::trace!("transport write: {} bytes", data.len());

        // Wait for writer to be ready (handles backpressure)
        JsFuture::from(self.writer.ready())
            .await
            .map_err(|e| SerprogError::IoError(format!("Writer not ready: {:?}", e)))?;

        // Write the data
        let array = js_sys::Uint8Array::from(data);
        let write_promise = self.writer.write_with_chunk(&array);
        JsFuture::from(write_promise)
            .await
            .map_err(|e| SerprogError::IoError(format!("Write failed: {:?}", e)))?;

        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<()> {
        log::trace!("transport read: requesting {} bytes", buf.len());
        let mut offset = 0;

        // First, drain any buffered data
        if !self.read_buffer.is_empty() {
            let to_copy = std::cmp::min(self.read_buffer.len(), buf.len());
            buf[..to_copy].copy_from_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            offset = to_copy;
            log::trace!("  drained {} bytes from buffer", to_copy);
        }

        // Read more chunks until we have enough
        while offset < buf.len() {
            log::trace!("  reading chunk, have {}/{} bytes", offset, buf.len());
            let chunk = self.read_chunk().await?;
            log::trace!("  got chunk of {} bytes", chunk.len());
            let remaining = buf.len() - offset;

            if chunk.len() <= remaining {
                buf[offset..offset + chunk.len()].copy_from_slice(&chunk);
                offset += chunk.len();
            } else {
                // Copy what we need, buffer the rest
                buf[offset..].copy_from_slice(&chunk[..remaining]);
                self.read_buffer.extend_from_slice(&chunk[remaining..]);
                offset = buf.len();
            }
        }

        log::trace!("transport read: complete");
        Ok(())
    }

    async fn read_nonblock(&mut self, buf: &mut [u8], _timeout_ms: u32) -> Result<usize> {
        // For WebSerial, we don't have true non-blocking reads with timeout
        // We'll try to read what's available in our buffer first
        if !self.read_buffer.is_empty() {
            let to_copy = std::cmp::min(self.read_buffer.len(), buf.len());
            buf[..to_copy].copy_from_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            return Ok(to_copy);
        }

        // For now, do a blocking read - in the future we could use AbortController
        // with a timeout to implement true non-blocking behavior
        let chunk = self.read_chunk().await?;
        let to_copy = std::cmp::min(chunk.len(), buf.len());
        buf[..to_copy].copy_from_slice(&chunk[..to_copy]);

        // Buffer excess
        if chunk.len() > to_copy {
            self.read_buffer.extend_from_slice(&chunk[to_copy..]);
        }

        Ok(to_copy)
    }

    async fn write_nonblock(&mut self, data: &[u8], _timeout_ms: u32) -> Result<bool> {
        // WebSerial writes are always "blocking" in the async sense
        self.write(data).await?;
        Ok(true)
    }

    async fn flush(&mut self) -> Result<()> {
        // WebSerial doesn't have an explicit flush, writes are immediate
        Ok(())
    }
}
