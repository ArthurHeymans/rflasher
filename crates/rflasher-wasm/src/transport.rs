//! WebSerial transport implementation for browser-based serprog communication
//!
//! This module provides a WebSerial-based transport that implements the
//! `Transport` trait from rflasher-serprog for async mode.

use js_sys::{Object, Reflect, Uint8Array};
use maybe_async::maybe_async;
use rflasher_serprog::error::{Result, SerprogError};
use rflasher_serprog::Transport;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

// WebSerial API bindings (not yet in stable web-sys)
#[wasm_bindgen]
extern "C" {
    /// Navigator.serial
    #[wasm_bindgen(js_namespace = navigator, js_name = serial)]
    pub static SERIAL: Serial;

    /// Serial interface
    pub type Serial;

    #[wasm_bindgen(method, catch, js_name = requestPort)]
    pub async fn request_port(this: &Serial) -> std::result::Result<JsValue, JsValue>;

    /// SerialPort interface
    pub type SerialPort;

    #[wasm_bindgen(method, catch)]
    pub async fn open(this: &SerialPort, options: &JsValue) -> std::result::Result<(), JsValue>;

    #[wasm_bindgen(method, catch)]
    pub async fn close(this: &SerialPort) -> std::result::Result<(), JsValue>;

    #[wasm_bindgen(method, getter)]
    pub fn readable(this: &SerialPort) -> Option<web_sys::ReadableStream>;

    #[wasm_bindgen(method, getter)]
    pub fn writable(this: &SerialPort) -> Option<web_sys::WritableStream>;

    /// ReadableStreamDefaultReader
    pub type ReadableStreamDefaultReader;

    #[wasm_bindgen(method, catch)]
    pub async fn read(this: &ReadableStreamDefaultReader) -> std::result::Result<JsValue, JsValue>;

    #[wasm_bindgen(method, js_name = releaseLock)]
    pub fn release_lock(this: &ReadableStreamDefaultReader);

    /// WritableStreamDefaultWriter
    pub type WritableStreamDefaultWriter;

    #[wasm_bindgen(method, catch, js_name = write)]
    pub async fn write_chunk(
        this: &WritableStreamDefaultWriter,
        chunk: &JsValue,
    ) -> std::result::Result<(), JsValue>;

    #[wasm_bindgen(method, js_name = releaseLock)]
    pub fn release_lock_writer(this: &WritableStreamDefaultWriter);
}

/// WebSerial transport for browser-based serprog communication
pub struct WebSerialTransport {
    #[allow(dead_code)]
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
        // Request a port from the user
        let port: SerialPort = SERIAL
            .request_port()
            .await
            .map_err(|e| {
                SerprogError::ConnectionFailed(format!("Failed to request port: {:?}", e))
            })?
            .unchecked_into();

        Self::open_port(port, baud_rate).await
    }

    /// Open an already-selected serial port
    pub async fn open_port(port: SerialPort, baud_rate: u32) -> Result<Self> {
        // Configure port options
        let options = js_sys::Object::new();
        Reflect::set(&options, &"baudRate".into(), &baud_rate.into())
            .map_err(|_| SerprogError::ConnectionFailed("Failed to set baudRate".to_string()))?;

        // Open the port
        port.open(&options.into())
            .await
            .map_err(|e| SerprogError::ConnectionFailed(format!("Failed to open port: {:?}", e)))?;

        // Get reader and writer
        let readable = port
            .readable()
            .ok_or_else(|| SerprogError::ConnectionFailed("Port not readable".to_string()))?;
        let reader: ReadableStreamDefaultReader = readable.get_reader().unchecked_into();

        let writable = port
            .writable()
            .ok_or_else(|| SerprogError::ConnectionFailed("Port not writable".to_string()))?;
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
        self.release_writer_lock();

        // Close the port
        self.port.close().await.map_err(|e| {
            SerprogError::ConnectionFailed(format!("Failed to close port: {:?}", e))
        })?;

        log::info!("WebSerial port closed");
        Ok(())
    }

    fn release_writer_lock(&self) {
        self.writer.release_lock_writer();
    }

    /// Read a chunk from the stream
    async fn read_chunk(&mut self) -> Result<Vec<u8>> {
        let result = self
            .reader
            .read()
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

        let array: Uint8Array = value
            .dyn_into()
            .map_err(|_| SerprogError::IoError("Value is not Uint8Array".to_string()))?;

        Ok(array.to_vec())
    }
}

#[maybe_async(AFIT)]
impl Transport for WebSerialTransport {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let array = Uint8Array::from(data);
        self.writer
            .write_chunk(&array.into())
            .await
            .map_err(|e| SerprogError::IoError(format!("Write failed: {:?}", e)))?;
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut offset = 0;

        // First, drain any buffered data
        if !self.read_buffer.is_empty() {
            let to_copy = std::cmp::min(self.read_buffer.len(), buf.len());
            buf[..to_copy].copy_from_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            offset = to_copy;
        }

        // Read more chunks until we have enough
        while offset < buf.len() {
            let chunk = self.read_chunk().await?;
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
