//! WebSerial transport implementation for browser-based serprog communication
//!
//! This module provides a WebSerial-based transport that implements the
//! `Transport` trait from rflasher-serprog for async mode.
//!
//! Uses web-sys bindings for the WebSerial API types.

// Allow deprecated JsStatic - single-threaded WASM doesn't need thread_local_v2
#![allow(deprecated)]

use maybe_async::maybe_async;
use rflasher_serprog::error::{Result, SerprogError};
use rflasher_serprog::Transport;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FlowControlType, ReadableStream, ReadableStreamByobReader, ReadableStreamGetReaderOptions,
    ReadableStreamReaderMode, SerialOptions, SerialPort, WritableStreamDefaultWriter,
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
    reader: ReadableStreamByobReader,
    writer: WritableStreamDefaultWriter,
    /// Buffer for excess bytes read from the stream
    read_buffer: Vec<u8>,
    /// Total bytes read (for debugging)
    total_read: usize,
    /// Total bytes written (for debugging)
    total_written: usize,
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

        // TODO the code hangs at some point with relationship to this size (around 16x bytes read)
        // Set this very large so you're not so likely to encounter it
        options.set_buffer_size(16 * 1024 * 1024);

        // Use hardware flow control if the device supports it
        options.set_flow_control(FlowControlType::Hardware);

        log::info!(
            "Opening port with baudRate={}, bufferSize=256, flowControl=hardware, BYOB reader",
            baud_rate
        );

        // Open the port - SerialPort::open returns a Promise
        let open_promise = port.open(&options);
        JsFuture::from(open_promise)
            .await
            .map_err(|e| SerprogError::ConnectionFailed(format!("Failed to open port: {:?}", e)))?;

        // Get BYOB reader from readable stream for precise read control
        let readable: ReadableStream = port.readable();
        let reader_options = ReadableStreamGetReaderOptions::new();
        reader_options.set_mode(ReadableStreamReaderMode::Byob);
        let reader: ReadableStreamByobReader = readable
            .get_reader_with_options(&reader_options)
            .unchecked_into();

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
            total_read: 0,
            total_written: 0,
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

    /// Read into a buffer using BYOB reader
    ///
    /// Returns the number of bytes read and the new view (BYOB transfers ownership)
    async fn read_byob(&mut self, view: js_sys::Uint8Array) -> Result<(usize, js_sys::Uint8Array)> {
        // BYOB reader's read takes an ArrayBufferView and returns Promise<{value, done}>
        let read_promise = self.reader.read_with_array_buffer_view(&view);
        let result = JsFuture::from(read_promise)
            .await
            .map_err(|e| SerprogError::IoError(format!("BYOB read failed: {:?}", e)))?;

        // Get 'done' property
        let done = js_sys::Reflect::get(&result, &"done".into())
            .map_err(|_| SerprogError::IoError("Failed to get done property".to_string()))?
            .as_bool()
            .unwrap_or(false);

        if done {
            return Err(SerprogError::IoError("Stream ended".to_string()));
        }

        // Get 'value' property - this is the filled ArrayBufferView
        let value = js_sys::Reflect::get(&result, &"value".into())
            .map_err(|_| SerprogError::IoError("Failed to get value property".to_string()))?;

        if value.is_undefined() {
            return Err(SerprogError::IoError(
                "No value in BYOB read result".to_string(),
            ));
        }

        let new_view: js_sys::Uint8Array = value
            .dyn_into()
            .map_err(|_| SerprogError::IoError("Value is not Uint8Array".to_string()))?;

        // The returned view has byteLength set to actual bytes read
        let bytes_read = new_view.byte_length() as usize;

        Ok((bytes_read, new_view))
    }
}

#[maybe_async(AFIT)]
impl Transport for WebSerialTransport {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        log::info!(
            "transport write: {} bytes (total written: {})",
            data.len(),
            self.total_written
        );

        // Wait for writer to be ready (handles backpressure)
        log::trace!("  waiting for writer.ready()");
        JsFuture::from(self.writer.ready())
            .await
            .map_err(|e| SerprogError::IoError(format!("Writer not ready: {:?}", e)))?;
        log::trace!("  writer ready, writing");

        // Write the data
        let array = js_sys::Uint8Array::from(data);
        let write_promise = self.writer.write_with_chunk(&array);
        JsFuture::from(write_promise)
            .await
            .map_err(|e| SerprogError::IoError(format!("Write failed: {:?}", e)))?;

        self.total_written += data.len();
        log::trace!("  write complete");
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<()> {
        log::info!(
            "transport read: requesting {} bytes (total read: {})",
            buf.len(),
            self.total_read
        );
        let mut offset = 0;

        // First, drain any buffered data
        if !self.read_buffer.is_empty() {
            let to_copy = std::cmp::min(self.read_buffer.len(), buf.len());
            buf[..to_copy].copy_from_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            offset = to_copy;
            log::trace!("  drained {} bytes from buffer", to_copy);
        }

        // Read more data using BYOB reader until we have enough
        while offset < buf.len() {
            let remaining = buf.len() - offset;
            log::trace!("  BYOB read, need {}/{} bytes", remaining, buf.len());

            // Create a view for exactly what we need
            let view = js_sys::Uint8Array::new_with_length(remaining as u32);
            log::trace!("  calling read_byob...");
            let (bytes_read, filled_view) = self.read_byob(view).await?;
            log::trace!("  BYOB got {} bytes", bytes_read);

            if bytes_read == 0 {
                return Err(SerprogError::IoError("Read returned 0 bytes".to_string()));
            }

            // Copy from the filled view to our output buffer
            filled_view.copy_to(&mut buf[offset..offset + bytes_read]);
            offset += bytes_read;
        }

        self.total_read += buf.len();
        log::trace!("transport read: complete");
        Ok(())
    }

    async fn read_nonblock(&mut self, buf: &mut [u8], _timeout_ms: u32) -> Result<usize> {
        log::trace!("transport read_nonblock: requesting {} bytes", buf.len());
        let mut offset = 0;

        // First, drain any buffered data
        if !self.read_buffer.is_empty() {
            let to_copy = std::cmp::min(self.read_buffer.len(), buf.len());
            buf[..to_copy].copy_from_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            offset = to_copy;
            log::trace!("  drained {} bytes from buffer", to_copy);
        }

        // If buffer is not full yet, do one BYOB read
        if offset < buf.len() {
            let remaining = buf.len() - offset;
            log::trace!("  BYOB read_nonblock, need {} bytes", remaining);

            // Create a view for what we need
            let view = js_sys::Uint8Array::new_with_length(remaining as u32);
            let (bytes_read, filled_view) = self.read_byob(view).await?;
            log::trace!("  BYOB got {} bytes", bytes_read);

            // Copy from the filled view to our output buffer
            if bytes_read > 0 {
                filled_view.copy_to(&mut buf[offset..offset + bytes_read]);
                offset += bytes_read;
            }
        }

        log::trace!("transport read_nonblock: complete, read {} bytes", offset);
        Ok(offset)
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
