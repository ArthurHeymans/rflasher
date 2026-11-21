//! Shared async state, progress reporting, and UI status types.

use eframe::egui;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use rflasher_core::chip::FlashChip;
use rflasher_core::flash::ProbeResult;
use rflasher_core::flash::unified::{WriteProgress, WriteStats};

use crate::programmer::Programmer;

// =============================================================================
// Browser yield helper
// =============================================================================

/// Yield control to the browser's event loop.
/// This is important in WASM to prevent the async runtime from starving
/// the browser's event handling (which WebSerial depends on).
pub(crate) async fn yield_to_browser() {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        // setTimeout(resolve, 0) yields to the event loop
        let window = web_sys::window().unwrap();
        window
            .set_timeout_with_callback(&resolve)
            .expect("setTimeout failed");
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

// Shared State for async task communication
// =============================================================================

/// Messages from async tasks to the UI
#[derive(Debug)]
pub(crate) enum AsyncMessage {
    /// Log message
    Log(LogLevel, String),
    /// Connection established
    Connected { programmer_name: String },
    /// Connection failed
    ConnectionFailed(String),
    /// Probe completed
    ProbeComplete(Box<ProbeResult>),
    /// Probe failed
    ProbeFailed(String),
    /// Read completed
    ReadComplete(Vec<u8>),
    /// Read failed
    ReadFailed(String),
    /// Write completed
    WriteComplete(WriteStats),
    /// Write failed
    WriteFailed(String),
    /// Erase completed
    EraseComplete,
    /// Erase failed
    EraseFailed(String),
    /// Verify completed
    VerifyComplete,
    /// Verify failed
    VerifyFailed(String),
    /// Progress update
    Progress(ProgressUpdate),
    /// Operation cancelled/disconnected
    Disconnected,
}

/// Progress update from async operations
#[derive(Debug, Clone)]
pub(crate) enum ProgressUpdate {
    Reading { done: usize, total: usize },
    Erasing { done: usize, total: usize },
    Writing { done: usize, total: usize },
    Verifying { done: usize, total: usize },
}

/// Shared state between UI and async tasks
#[derive(Default)]
pub(crate) struct SharedState {
    /// Messages from async tasks
    pub(crate) messages: Vec<AsyncMessage>,
    /// The connected programmer (if any)
    pub(crate) programmer: Option<Programmer>,
    /// Whether an async operation is running
    pub(crate) busy: bool,
}

pub(crate) type SharedStateRef = Rc<RefCell<SharedState>>;

// =============================================================================
// Progress reporter for async operations
// =============================================================================

/// Progress reporter that sends updates to the shared state
pub(crate) struct SharedProgress {
    state: SharedStateRef,
    ctx: Option<egui::Context>,
    total_read: usize,
    total_erase: usize,
    total_write: usize,
    last_repaint_bytes: usize,
}

/// Minimum bytes between repaint requests (64KB) to avoid overwhelming the browser
const REPAINT_THROTTLE_BYTES: usize = 65536;

impl SharedProgress {
    pub(crate) fn new(state: SharedStateRef, ctx: Option<egui::Context>) -> Self {
        Self {
            state,
            ctx,
            total_read: 0,
            total_erase: 0,
            total_write: 0,
            last_repaint_bytes: 0,
        }
    }

    fn request_repaint(&self) {
        if let Some(ref ctx) = self.ctx {
            ctx.request_repaint();
        }
    }

    /// Request repaint only if enough progress has been made (throttled)
    fn request_repaint_throttled(&mut self, current_bytes: usize) {
        if current_bytes >= self.last_repaint_bytes + REPAINT_THROTTLE_BYTES {
            self.last_repaint_bytes = current_bytes;
            self.request_repaint();
        }
    }

    /// Reset throttle state for a new operation
    fn reset_throttle(&mut self) {
        self.last_repaint_bytes = 0;
    }
}

impl WriteProgress for SharedProgress {
    fn reading(&mut self, total_bytes: usize) {
        self.total_read = total_bytes;
        self.reset_throttle();
        self.state
            .borrow_mut()
            .messages
            .push(AsyncMessage::Progress(ProgressUpdate::Reading {
                done: 0,
                total: total_bytes,
            }));
        self.request_repaint();
    }

    fn read_progress(&mut self, bytes_read: usize) {
        self.state
            .borrow_mut()
            .messages
            .push(AsyncMessage::Progress(ProgressUpdate::Reading {
                done: bytes_read,
                total: self.total_read,
            }));
        self.request_repaint_throttled(bytes_read);
    }

    fn erasing(&mut self, _blocks_to_erase: usize, bytes_to_erase: usize) {
        self.total_erase = bytes_to_erase;
        self.reset_throttle();
        self.state
            .borrow_mut()
            .messages
            .push(AsyncMessage::Progress(ProgressUpdate::Erasing {
                done: 0,
                total: bytes_to_erase,
            }));
        self.request_repaint();
    }

    fn erase_progress(&mut self, _blocks_erased: usize, bytes_erased: usize) {
        self.state
            .borrow_mut()
            .messages
            .push(AsyncMessage::Progress(ProgressUpdate::Erasing {
                done: bytes_erased,
                total: self.total_erase,
            }));
        self.request_repaint_throttled(bytes_erased);
    }

    fn writing(&mut self, bytes_to_write: usize) {
        self.total_write = bytes_to_write;
        self.reset_throttle();
        self.state
            .borrow_mut()
            .messages
            .push(AsyncMessage::Progress(ProgressUpdate::Writing {
                done: 0,
                total: bytes_to_write,
            }));
        self.request_repaint();
    }

    fn write_progress(&mut self, bytes_written: usize) {
        self.state
            .borrow_mut()
            .messages
            .push(AsyncMessage::Progress(ProgressUpdate::Writing {
                done: bytes_written,
                total: self.total_write,
            }));
        self.request_repaint_throttled(bytes_written);
    }

    fn complete(&mut self, _stats: &WriteStats) {
        // Completion is handled by the operation-specific message
    }
}

/// Detected chip information
#[derive(Clone)]
pub(crate) struct ChipInfo {
    pub(crate) chip: FlashChip,
    pub(crate) size: u32,
    #[allow(dead_code)]
    pub(crate) from_database: bool,
}

/// Connection state
#[derive(Default)]
pub(crate) enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected {
        programmer_name: String,
    },
}

/// Current operation state
#[derive(Default, Clone)]
pub(crate) enum OperationState {
    #[default]
    Idle,
    Probing,
    Reading {
        bytes_done: usize,
        bytes_total: usize,
    },
    Writing {
        bytes_done: usize,
        bytes_total: usize,
        phase: WritePhase,
    },
    Erasing {
        bytes_done: usize,
        bytes_total: usize,
    },
    Verifying {
        bytes_done: usize,
        bytes_total: usize,
    },
}

#[derive(Default, Clone)]
pub(crate) enum WritePhase {
    #[default]
    Reading,
    Erasing,
    Writing,
}

/// Status log
pub(crate) struct StatusLog {
    pub(crate) messages: VecDeque<(LogLevel, String)>,
    max_messages: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl Default for StatusLog {
    fn default() -> Self {
        Self {
            messages: VecDeque::new(),
            max_messages: 100,
        }
    }
}

impl StatusLog {
    pub(crate) fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.messages.push_back((level, message.into()));
        if self.messages.len() > self.max_messages {
            self.messages.pop_front();
        }
    }

    pub(crate) fn info(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    pub(crate) fn success(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Success, message);
    }

    pub(crate) fn warn(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Warning, message);
    }

    pub(crate) fn error(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }
}
