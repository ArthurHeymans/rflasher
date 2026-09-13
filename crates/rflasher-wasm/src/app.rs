//! Main egui application for rflasher web interface

use eframe::egui;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use rflasher_chips::ChipDatabase;
use rflasher_core::chip::FlashChip;
use rflasher_core::flash::unified::{WriteProgress, WriteStats, smart_write};
use rflasher_core::flash::{
    FlashContext, FlashDevice, HybridFlashDevice, ProbeResult, SpiFlashDevice,
};
use rflasher_programmers::catalog::{OptionKind, ProgrammerInfo, Transport, available_programmers};
use rflasher_programmers::ch341a::Ch341a;
use rflasher_programmers::ch347::Ch347;
use rflasher_programmers::dediprog::Dediprog;
use rflasher_programmers::ft4222::Ft4222;
use rflasher_programmers::ftdi::Ftdi;
use rflasher_programmers::raiden::RaidenDebugSpi;
use rflasher_programmers::serprog::Serprog;

use crate::form::{Field, ProgrammerForm, borrow_pairs, int_error, is_web_transport, speed_label};
use crate::transport::WebSerialTransport;

// =============================================================================
// Browser yield helper
// =============================================================================

/// Yield control to the browser's event loop.
/// This is important in WASM to prevent the async runtime from starving
/// the browser's event handling (which WebSerial depends on).
async fn yield_to_browser() {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        // setTimeout(resolve, 0) yields to the event loop
        let window = web_sys::window().unwrap();
        window
            .set_timeout_with_callback(&resolve)
            .expect("setTimeout failed");
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

// =============================================================================
// Programmer abstraction
// =============================================================================

/// Baud rate used for serprog when the form leaves `baud=` unset.
const DEFAULT_SERPROG_BAUD: u32 = 115200;

/// Connected programmer - wraps a serprog, CH341A, CH347, FTDI, FT4222H,
/// Dediprog, or Raiden device
#[allow(clippy::large_enum_variant)]
enum Programmer {
    Serprog(Serprog<WebSerialTransport>),
    Ch341a(Ch341a),
    Ch347(Ch347),
    Ftdi(Ftdi),
    Ft4222(Ft4222),
    Dediprog(Dediprog),
    Raiden(RaidenDebugSpi),
}

// ---------------------------------------------------------------------------
// Macro to dispatch operations across programmer variants
// ---------------------------------------------------------------------------
// This eliminates the per-variant match arm duplication in every spawner.
// The macro takes the programmer, calls the operation on the inner SpiMaster,
// and puts it back into the shared state.

/// Dispatch an async operation across all programmer variants.
///
/// Usage: `with_programmer!(shared, programmer, |master| { async body using master })`
///
/// The `master` binding is `&mut impl SpiMaster`. The async body must return
/// the master back via `device.into_parts()` pattern or similar -- the macro
/// handles putting the Programmer wrapper back into shared state.
macro_rules! with_programmer {
    ($shared:expr, $programmer:expr, $name:ident, $body:expr) => {
        match $programmer {
            Programmer::Serprog(mut $name) => {
                let result = $body;
                $shared.borrow_mut().programmer = Some(Programmer::Serprog($name));
                result
            }
            Programmer::Ch341a(mut $name) => {
                let result = $body;
                $shared.borrow_mut().programmer = Some(Programmer::Ch341a($name));
                result
            }
            Programmer::Ch347(mut $name) => {
                let result = $body;
                $shared.borrow_mut().programmer = Some(Programmer::Ch347($name));
                result
            }
            Programmer::Ftdi(mut $name) => {
                let result = $body;
                $shared.borrow_mut().programmer = Some(Programmer::Ftdi($name));
                result
            }
            Programmer::Ft4222(mut $name) => {
                let result = $body;
                $shared.borrow_mut().programmer = Some(Programmer::Ft4222($name));
                result
            }
            Programmer::Dediprog(mut $name) => {
                let result = $body;
                $shared.borrow_mut().programmer = Some(Programmer::Dediprog($name));
                result
            }
            Programmer::Raiden(mut $name) => {
                let result = $body;
                $shared.borrow_mut().programmer = Some(Programmer::Raiden($name));
                result
            }
        }
    };
}

/// Like [`with_programmer!`], but wraps the programmer in a [`FlashDevice`]
/// for operations that need chip-level read/write/erase.
///
/// For Dediprog: creates [`HybridFlashDevice`] (fast bulk read/write via
/// `OpaqueMaster`) and calls `set_flash_size()` first.
/// For all others: creates [`SpiFlashDevice`].
///
/// The body receives `$device` as `&mut impl FlashDevice`. The macro handles
/// extracting the master back via `into_parts()` and putting the Programmer
/// wrapper back into shared state.
macro_rules! with_flash_device {
    ($shared:expr, $programmer:expr, $ctx_flash:expr, $device:ident, $body:expr) => {
        match $programmer {
            Programmer::Serprog(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                let result = { $body };
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Serprog(master));
                result
            }
            Programmer::Ch341a(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                let result = { $body };
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ch341a(master));
                result
            }
            Programmer::Ch347(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                let result = { $body };
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ch347(master));
                result
            }
            Programmer::Ftdi(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                let result = { $body };
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ftdi(master));
                result
            }
            Programmer::Ft4222(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                let result = { $body };
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Ft4222(master));
                result
            }
            Programmer::Dediprog(mut master) => {
                master.set_flash_size($ctx_flash.total_size() as u32);
                let mut $device = HybridFlashDevice::new(master, $ctx_flash);
                let result = { $body };
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Dediprog(master));
                result
            }
            Programmer::Raiden(master) => {
                let mut $device = SpiFlashDevice::new(master, $ctx_flash);
                let result = { $body };
                let (master, _) = $device.into_parts();
                $shared.borrow_mut().programmer = Some(Programmer::Raiden(master));
                result
            }
        }
    };
}

// =============================================================================
// Connecting
// =============================================================================

/// Ask the browser for a device and open programmer `name` with `options`.
///
/// `options` are the raw `key=value` pairs from the form; each backend's own
/// `parse_options` turns them into its typed config, exactly as the CLI does.
/// Returns the opened programmer and a display name for the status panel.
async fn open_programmer(
    name: &str,
    options: &[(&str, &str)],
) -> Result<(Programmer, String), String> {
    match name {
        "serprog" => {
            let config = rflasher_programmers::serprog::parse_options(options)?;
            let baud = config.baud.unwrap_or(DEFAULT_SERPROG_BAUD);
            let transport = WebSerialTransport::request_and_open(baud)
                .await
                .map_err(|e| e.to_string())?;
            let mut serprog = Serprog::new(transport).await.map_err(|e| e.to_string())?;
            if let Some(khz) = config.spispeed_khz
                && let Err(e) = serprog.set_spi_speed(khz * 1000).await
            {
                log::warn!("Failed to set SPI speed: {e}");
            }
            if let Some(cs) = config.cs {
                serprog
                    .set_spi_cs(cs)
                    .await
                    .map_err(|e| format!("Failed to set chip select: {e}"))?;
            }
            let display = serprog.info().name_str().to_string();
            Ok((Programmer::Serprog(serprog), display))
        }
        "ch341a" => {
            let device_info = Ch341a::request_device().await.map_err(|e| e.to_string())?;
            let ch341a = Ch341a::open(device_info).await.map_err(|e| e.to_string())?;
            Ok((Programmer::Ch341a(ch341a), "CH341A".to_string()))
        }
        "ch347" => {
            let config =
                rflasher_programmers::ch347::parse_options(options).map_err(|e| e.to_string())?;
            let device_info = Ch347::request_device().await.map_err(|e| e.to_string())?;
            let ch347 = Ch347::open_with_config(device_info, config)
                .await
                .map_err(|e| e.to_string())?;
            let display = match ch347.variant() {
                rflasher_programmers::ch347::Ch347Variant::Ch347T => "CH347T",
                rflasher_programmers::ch347::Ch347Variant::Ch347F => "CH347F",
            };
            Ok((Programmer::Ch347(ch347), display.to_string()))
        }
        "ftdi" => {
            let config =
                rflasher_programmers::ftdi::parse_options(options).map_err(|e| e.to_string())?;
            let device = Ftdi::request_device().await.map_err(|e| e.to_string())?;
            let ftdi = Ftdi::open(device, &config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!(
                "{} ch {}",
                config.device_type.name(),
                config.interface.letter()
            );
            Ok((Programmer::Ftdi(ftdi), display))
        }
        "ft4222" => {
            let config =
                rflasher_programmers::ft4222::parse_options(options).map_err(|e| e.to_string())?;
            let device_info = Ft4222::request_device().await.map_err(|e| e.to_string())?;
            let ft4222 = Ft4222::open(device_info, config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!("FT4222H @ {} kHz", ft4222.actual_speed_khz());
            Ok((Programmer::Ft4222(ft4222), display))
        }
        "dediprog" => {
            let config = rflasher_programmers::dediprog::parse_options(options)
                .map_err(|e| e.to_string())?;
            let device_info = Dediprog::request_device()
                .await
                .map_err(|e| e.to_string())?;
            let dediprog = Dediprog::open(device_info, config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!("Dediprog {}", dediprog.device_string());
            Ok((Programmer::Dediprog(dediprog), display))
        }
        "raiden_debug_spi" => {
            let config =
                rflasher_programmers::raiden::parse_options(options).map_err(|e| e.to_string())?;
            let device_info = RaidenDebugSpi::request_device()
                .await
                .map_err(|e| e.to_string())?;
            let raiden = RaidenDebugSpi::open(device_info, &config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!("Raiden ({})", config.target);
            Ok((Programmer::Raiden(raiden), display))
        }
        other => Err(format!("{other} cannot be opened from a browser")),
    }
}

// =============================================================================
// Shared State for async task communication
// =============================================================================

/// Messages from async tasks to the UI
#[derive(Debug)]
enum AsyncMessage {
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
enum ProgressUpdate {
    Reading { done: usize, total: usize },
    Erasing { done: usize, total: usize },
    Writing { done: usize, total: usize },
    Verifying { done: usize, total: usize },
}

/// Shared state between UI and async tasks
#[derive(Default)]
struct SharedState {
    /// Messages from async tasks
    messages: Vec<AsyncMessage>,
    /// The connected programmer (if any)
    programmer: Option<Programmer>,
    /// Whether an async operation is running
    busy: bool,
}

type SharedStateRef = Rc<RefCell<SharedState>>;

// =============================================================================
// Progress reporter for async operations
// =============================================================================

/// Progress reporter that sends updates to the shared state
struct SharedProgress {
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
    fn new(state: SharedStateRef, ctx: Option<egui::Context>) -> Self {
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

// =============================================================================
// Application State
// =============================================================================

/// Application state
pub struct RflasherApp {
    /// Shared state with async tasks
    shared: SharedStateRef,
    /// Current connection state (UI view)
    connection: ConnectionState,
    /// Operation state (what we're currently doing)
    operation: OperationState,
    /// Status messages
    status: StatusLog,
    /// File data for read/write operations
    file_buffer: Option<Vec<u8>>,
    /// Programmers the browser can open, from the shared catalog
    programmers: Vec<ProgrammerInfo>,
    /// Option values for the selected programmer
    form: ProgrammerForm,
    /// Chip database
    chip_db: ChipDatabase,
    /// Detected chip info
    chip_info: Option<ChipInfo>,
    /// egui context for requesting repaints
    ctx: Option<egui::Context>,
    /// Whether the udev rules window is open
    show_udev_window: bool,
}

/// Detected chip information
#[derive(Clone)]
struct ChipInfo {
    chip: FlashChip,
    size: u32,
    #[allow(dead_code)]
    from_database: bool,
}

/// Connection state
#[derive(Default)]
enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected {
        programmer_name: String,
    },
}

/// Current operation state
#[derive(Default, Clone)]
enum OperationState {
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
enum WritePhase {
    #[default]
    Reading,
    Erasing,
    Writing,
}

/// Status log
struct StatusLog {
    messages: VecDeque<(LogLevel, String)>,
    max_messages: usize,
}

#[derive(Clone, Copy, Debug)]
enum LogLevel {
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
    fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.messages.push_back((level, message.into()));
        if self.messages.len() > self.max_messages {
            self.messages.pop_front();
        }
    }

    fn info(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    fn success(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Success, message);
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Warning, message);
    }

    fn error(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }
}

impl Default for RflasherApp {
    fn default() -> Self {
        let programmers: Vec<ProgrammerInfo> = available_programmers()
            .into_iter()
            .filter(|p| is_web_transport(p.transport))
            .collect();
        let form = programmers
            .first()
            .map(ProgrammerForm::new)
            .expect("wasm build must enable at least one browser-capable programmer");
        Self {
            shared: Rc::new(RefCell::new(SharedState::default())),
            connection: ConnectionState::Disconnected,
            operation: OperationState::Idle,
            status: StatusLog::default(),
            file_buffer: None,
            programmers,
            form,
            chip_db: ChipDatabase::new(),
            chip_info: None,
            ctx: None,
            show_udev_window: false,
        }
    }
}

impl RflasherApp {
    /// Create a new application
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    fn is_busy(&self) -> bool {
        !matches!(self.operation, OperationState::Idle)
    }

    fn is_connected(&self) -> bool {
        matches!(self.connection, ConnectionState::Connected { .. })
    }

    fn chip_detected(&self) -> bool {
        self.chip_info.is_some()
    }

    /// Catalog entry for the programmer currently selected in the form.
    fn selected_info(&self) -> Option<&ProgrammerInfo> {
        self.programmers.iter().find(|p| p.name == self.form.name)
    }

    /// Whether the selected programmer is opened through WebUSB (vs WebSerial).
    fn selected_is_webusb(&self) -> bool {
        self.selected_info()
            .is_some_and(|info| info.transport == Transport::Usb)
    }

    /// Process messages from async tasks
    fn process_messages(&mut self) {
        let messages: Vec<AsyncMessage> = {
            let mut shared = self.shared.borrow_mut();
            std::mem::take(&mut shared.messages)
        };

        for msg in messages {
            match msg {
                AsyncMessage::Log(level, text) => {
                    self.status.log(level, text);
                }
                AsyncMessage::Connected { programmer_name } => {
                    self.connection = ConnectionState::Connected {
                        programmer_name: programmer_name.clone(),
                    };
                    self.status
                        .success(format!("Connected to {}", programmer_name));
                }
                AsyncMessage::ConnectionFailed(err) => {
                    self.connection = ConnectionState::Disconnected;
                    self.status.error(format!("Connection failed: {}", err));
                    if self.selected_is_webusb() {
                        for hint in webusb_failure_hints(&err) {
                            self.status.warn(*hint);
                        }
                    }
                }
                AsyncMessage::ProbeComplete(result) => {
                    self.operation = OperationState::Idle;
                    let chip_name = result.chip.name.clone();
                    let size = result.chip.total_size;
                    self.chip_info = Some(ChipInfo {
                        chip: result.chip,
                        size,
                        from_database: result.from_database,
                    });
                    let source = if result.from_database {
                        "database"
                    } else {
                        "SFDP"
                    };
                    self.status.success(format!(
                        "Detected: {} ({} KB) [from {}]",
                        chip_name,
                        size / 1024,
                        source
                    ));
                    if !result.mismatches.is_empty() {
                        self.status.warn(format!(
                            "{} mismatch(es) between SFDP and database",
                            result.mismatches.len()
                        ));
                    }
                }
                AsyncMessage::ProbeFailed(err) => {
                    self.operation = OperationState::Idle;
                    self.chip_info = None;
                    self.status.error(format!("Probe failed: {}", err));
                }
                AsyncMessage::ReadComplete(data) => {
                    self.operation = OperationState::Idle;
                    let size = data.len();
                    self.file_buffer = Some(data);
                    self.status.success(format!("Read {} bytes", size));
                }
                AsyncMessage::ReadFailed(err) => {
                    self.operation = OperationState::Idle;
                    self.status.error(format!("Read failed: {}", err));
                }
                AsyncMessage::WriteComplete(stats) => {
                    self.operation = OperationState::Idle;
                    self.status.success(format!(
                        "Write complete: {} bytes written, {} erases",
                        stats.bytes_written, stats.erases_performed
                    ));
                }
                AsyncMessage::WriteFailed(err) => {
                    self.operation = OperationState::Idle;
                    self.status.error(format!("Write failed: {}", err));
                }
                AsyncMessage::EraseComplete => {
                    self.operation = OperationState::Idle;
                    self.status.success("Erase complete");
                }
                AsyncMessage::EraseFailed(err) => {
                    self.operation = OperationState::Idle;
                    self.status.error(format!("Erase failed: {}", err));
                }
                AsyncMessage::VerifyComplete => {
                    self.operation = OperationState::Idle;
                    self.status.success("Verification passed");
                }
                AsyncMessage::VerifyFailed(err) => {
                    self.operation = OperationState::Idle;
                    self.status.error(format!("Verify failed: {}", err));
                }
                AsyncMessage::Progress(update) => match update {
                    ProgressUpdate::Reading { done, total } => {
                        self.operation = OperationState::Reading {
                            bytes_done: done,
                            bytes_total: total,
                        };
                    }
                    ProgressUpdate::Erasing { done, total } => {
                        if let OperationState::Writing { bytes_total, .. } = &self.operation {
                            self.operation = OperationState::Writing {
                                bytes_done: done,
                                bytes_total: *bytes_total,
                                phase: WritePhase::Erasing,
                            };
                        } else {
                            self.operation = OperationState::Erasing {
                                bytes_done: done,
                                bytes_total: total,
                            };
                        }
                    }
                    ProgressUpdate::Writing { done, total } => {
                        if let OperationState::Writing { bytes_total, .. } = &self.operation {
                            self.operation = OperationState::Writing {
                                bytes_done: done,
                                bytes_total: *bytes_total,
                                phase: WritePhase::Writing,
                            };
                        } else {
                            self.operation = OperationState::Writing {
                                bytes_done: done,
                                bytes_total: total,
                                phase: WritePhase::Writing,
                            };
                        }
                    }
                    ProgressUpdate::Verifying { done, total } => {
                        self.operation = OperationState::Verifying {
                            bytes_done: done,
                            bytes_total: total,
                        };
                    }
                },
                AsyncMessage::Disconnected => {
                    self.connection = ConnectionState::Disconnected;
                    self.operation = OperationState::Idle;
                    self.chip_info = None;
                    self.status.info("Disconnected");
                }
            }
        }
    }

    // =========================================================================
    // Async operation spawners
    // =========================================================================

    fn spawn_connect(&mut self) {
        let Some(info) = self.selected_info() else {
            self.status.error("No programmer selected");
            return;
        };
        let name = info.name;
        let picker = if info.transport == Transport::Usb {
            "WebUSB"
        } else {
            "WebSerial"
        };
        let options = self.form.owned_pairs();
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();

        self.connection = ConnectionState::Connecting;
        self.status
            .info(format!("Requesting {name} device via {picker}..."));

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            let message = match open_programmer(name, &borrow_pairs(&options)).await {
                Ok((programmer, programmer_name)) => {
                    shared.borrow_mut().programmer = Some(programmer);
                    AsyncMessage::Connected { programmer_name }
                }
                Err(e) => AsyncMessage::ConnectionFailed(e),
            };

            let mut state = shared.borrow_mut();
            state.messages.push(message);
            state.busy = false;
            drop(state);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_disconnect(&mut self) {
        let shared = self.shared.clone();

        // Take the programmer out of shared state
        let programmer = shared.borrow_mut().programmer.take();

        if let Some(programmer) = programmer {
            let ctx = self.ctx.clone();

            wasm_bindgen_futures::spawn_local(async move {
                match programmer {
                    Programmer::Serprog(mut serprog) => {
                        serprog.shutdown().await;
                    }
                    Programmer::Ch341a(mut ch341a) => {
                        ch341a.shutdown().await;
                    }
                    Programmer::Ch347(mut ch347) => {
                        ch347.shutdown().await;
                    }
                    Programmer::Ftdi(mut ftdi) => {
                        ftdi.shutdown().await;
                    }
                    Programmer::Ft4222(mut ft4222) => {
                        ft4222.shutdown().await;
                    }
                    Programmer::Dediprog(mut dediprog) => {
                        dediprog.shutdown().await;
                    }
                    Programmer::Raiden(mut raiden) => {
                        raiden.shutdown().await;
                    }
                }

                shared
                    .borrow_mut()
                    .messages
                    .push(AsyncMessage::Disconnected);

                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
        } else {
            self.connection = ConnectionState::Disconnected;
            self.chip_info = None;
        }
    }

    fn spawn_probe(&mut self) {
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        let chip_db = self.chip_db.clone();

        self.operation = OperationState::Probing;
        self.status.info("Probing chip...");

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            let programmer = shared.borrow_mut().programmer.take();

            if let Some(programmer) = programmer {
                use rflasher_core::flash::probe_detailed;

                with_programmer!(shared, programmer, master, {
                    match probe_detailed(&mut master, &chip_db).await {
                        Ok(result) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::ProbeComplete(Box::new(result)));
                        }
                        Err(e) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::ProbeFailed(format!("{:?}", e)));
                        }
                    }
                });
            } else {
                shared
                    .borrow_mut()
                    .messages
                    .push(AsyncMessage::ProbeFailed("Not connected".to_string()));
            }

            shared.borrow_mut().busy = false;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_read(&mut self) {
        let Some(ref chip_info) = self.chip_info else {
            self.status.error("No chip detected");
            return;
        };

        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        let chip = chip_info.chip.clone();
        let size = chip_info.size as usize;

        self.operation = OperationState::Reading {
            bytes_done: 0,
            bytes_total: size,
        };
        self.status.info(format!("Reading {} bytes...", size));

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            let programmer = shared.borrow_mut().programmer.take();

            if let Some(programmer) = programmer {
                let ctx_flash = FlashContext::new(chip);
                let mut buf = vec![0u8; size];

                /// Read chunk size. Larger chunks reduce per-chunk overhead,
                /// but 2 MiB only gives ~8 updates for a 16 MiB flash. Use
                /// 1 MiB as a compromise so the UI refreshes roughly every 6%.
                const READ_CHUNK_SIZE: usize = 1024 * 1024;
                /// Yield to the browser after each read chunk so progress is
                /// visible promptly during long WebUSB reads.
                const YIELD_INTERVAL: usize = READ_CHUNK_SIZE;

                with_flash_device!(shared, programmer, ctx_flash, device, {
                    let total = buf.len();
                    let mut offset = 0usize;
                    let mut last_yield = 0usize;
                    let mut read_error: Option<rflasher_core::error::Error> = None;

                    while offset < total {
                        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, total - offset);
                        match device
                            .read(offset as u32, &mut buf[offset..offset + chunk_size])
                            .await
                        {
                            Ok(()) => {}
                            Err(e) => {
                                read_error = Some(e);
                                break;
                            }
                        }
                        offset += chunk_size;

                        shared.borrow_mut().messages.push(AsyncMessage::Progress(
                            ProgressUpdate::Reading {
                                done: offset,
                                total,
                            },
                        ));

                        if let Some(ref ctx) = ctx {
                            ctx.request_repaint();
                        }

                        if offset >= last_yield + YIELD_INTERVAL {
                            last_yield = offset;
                            yield_to_browser().await;
                        }
                    }

                    match read_error {
                        None => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::ReadComplete(buf));
                        }
                        Some(e) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::ReadFailed(format!("{:?}", e)));
                        }
                    }
                });
            } else {
                shared
                    .borrow_mut()
                    .messages
                    .push(AsyncMessage::ReadFailed("Not connected".to_string()));
            }

            shared.borrow_mut().busy = false;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_write(&mut self) {
        let Some(ref chip_info) = self.chip_info else {
            self.status.error("No chip detected");
            return;
        };

        let Some(ref data) = self.file_buffer else {
            self.status.error("No file loaded");
            return;
        };

        let chip_size = chip_info.size as usize;
        if data.len() != chip_size {
            self.status.error(format!(
                "File size ({}) doesn't match chip size ({})",
                data.len(),
                chip_size
            ));
            return;
        }

        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        let chip = chip_info.chip.clone();
        let data = data.clone();

        self.operation = OperationState::Writing {
            bytes_done: 0,
            bytes_total: chip_size,
            phase: WritePhase::Reading,
        };
        self.status.info(format!("Writing {} bytes...", chip_size));

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            let programmer = shared.borrow_mut().programmer.take();

            if let Some(programmer) = programmer {
                let ctx_flash = FlashContext::new(chip);
                let mut progress = SharedProgress::new(shared.clone(), ctx.clone());

                with_flash_device!(shared, programmer, ctx_flash, device, {
                    let result = smart_write(&mut device, &data, &mut progress).await;

                    match result {
                        Ok(stats) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::WriteComplete(stats));
                        }
                        Err(e) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::WriteFailed(format!("{:?}", e)));
                        }
                    }
                });
            } else {
                shared
                    .borrow_mut()
                    .messages
                    .push(AsyncMessage::WriteFailed("Not connected".to_string()));
            }

            shared.borrow_mut().busy = false;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_erase(&mut self) {
        let Some(ref chip_info) = self.chip_info else {
            self.status.error("No chip detected");
            return;
        };

        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        let chip = chip_info.chip.clone();
        let size = chip_info.size;

        self.operation = OperationState::Erasing {
            bytes_done: 0,
            bytes_total: size as usize,
        };
        self.status.info("Erasing chip...");

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            let programmer = shared.borrow_mut().programmer.take();

            if let Some(programmer) = programmer {
                let ctx_flash = FlashContext::new(chip);

                with_flash_device!(shared, programmer, ctx_flash, device, {
                    let result = device.erase(0, size).await;

                    match result {
                        Ok(()) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::EraseComplete);
                        }
                        Err(e) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::EraseFailed(format!("{:?}", e)));
                        }
                    }
                });
            } else {
                shared
                    .borrow_mut()
                    .messages
                    .push(AsyncMessage::EraseFailed("Not connected".to_string()));
            }

            shared.borrow_mut().busy = false;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_verify(&mut self) {
        let Some(ref chip_info) = self.chip_info else {
            self.status.error("No chip detected");
            return;
        };

        let Some(ref data) = self.file_buffer else {
            self.status.error("No file loaded");
            return;
        };

        let chip_size = chip_info.size as usize;
        if data.len() != chip_size {
            self.status.error(format!(
                "File size ({}) doesn't match chip size ({})",
                data.len(),
                chip_size
            ));
            return;
        }

        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        let chip = chip_info.chip.clone();
        let data = data.clone();

        self.operation = OperationState::Verifying {
            bytes_done: 0,
            bytes_total: chip_size,
        };
        self.status.info("Verifying...");

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            let programmer = shared.borrow_mut().programmer.take();

            if let Some(programmer) = programmer {
                let ctx_flash = FlashContext::new(chip);

                const CHUNK_SIZE: usize = 4096;
                const YIELD_INTERVAL: usize = 65536;

                with_flash_device!(shared, programmer, ctx_flash, device, {
                    let total = data.len();
                    let mut offset = 0usize;
                    let mut last_repaint = 0usize;
                    let mut last_yield = 0usize;
                    let mut verify_error: Option<String> = None;

                    while offset < total {
                        let chunk_size = core::cmp::min(CHUNK_SIZE, total - offset);
                        let mut buf = vec![0u8; chunk_size];

                        match device.read(offset as u32, &mut buf).await {
                            Ok(()) => {
                                let expected = &data[offset..offset + chunk_size];
                                if buf != expected {
                                    for (i, (a, b)) in buf.iter().zip(expected.iter()).enumerate() {
                                        if a != b {
                                            verify_error = Some(format!(
                                                "Mismatch at 0x{:X}: read 0x{:02X}, expected 0x{:02X}",
                                                offset + i,
                                                a,
                                                b
                                            ));
                                            break;
                                        }
                                    }
                                    break;
                                }
                            }
                            Err(e) => {
                                verify_error = Some(format!("Read error: {:?}", e));
                                break;
                            }
                        }

                        offset += chunk_size;

                        shared.borrow_mut().messages.push(AsyncMessage::Progress(
                            ProgressUpdate::Verifying {
                                done: offset,
                                total,
                            },
                        ));

                        if offset >= last_repaint + YIELD_INTERVAL {
                            last_repaint = offset;
                            if let Some(ref ctx) = ctx {
                                ctx.request_repaint();
                            }
                        }

                        if offset >= last_yield + YIELD_INTERVAL {
                            last_yield = offset;
                            yield_to_browser().await;
                        }
                    }

                    match verify_error {
                        None => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::VerifyComplete);
                        }
                        Some(err) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::VerifyFailed(err));
                        }
                    }
                });
            } else {
                shared
                    .borrow_mut()
                    .messages
                    .push(AsyncMessage::VerifyFailed("Not connected".to_string()));
            }

            shared.borrow_mut().busy = false;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }
}

// =============================================================================
// Schema-driven option widgets
// =============================================================================

/// Placeholder shown for an unset option that falls back to the backend default.
const UNSET_LABEL: &str = "(default)";

/// Render one form field as a `label | widget` grid row, choosing the widget
/// from the option kind. The value stays a raw string so the backend parser
/// remains the single source of truth.
/// Actionable hints for a failed WebUSB open.
///
/// `ftdi-nusb` reports a bare "Unable to claim interface" when Chrome cannot
/// take the interface. WebUSB has no API to detach a kernel driver (the native
/// path uses `detach_and_claim_interface`; the browser cannot), so on Linux this
/// usually means the OS driver still owns the device.
fn webusb_failure_hints(error: &str) -> &'static [&'static str] {
    const CLAIM_FAILED: &[&str] = &[
        "The USB interface could not be claimed. WebUSB cannot detach a kernel \
         driver, so on Linux the driver (usually ftdi_sio) likely holds it.",
        "Unbind it and reconnect, replacing 1-4 with the bus-port from `lsusb -t`:",
        "    echo -n \"1-4\" | sudo tee /sys/bus/usb/drivers/ftdi_sio/unbind",
        "To make that permanent, blacklist the driver (e.g. `blacklist ftdi_sio` \
         in /etc/modprobe.d/), or make sure no other tab or program is using the \
         device.",
        "See Help > USB permissions for details.",
    ];
    const OTHER: &[&str] = &[
        "On Linux, this may be a permissions issue, or a kernel driver may hold \
         the USB interface.",
        "Check Help > USB permissions for udev rules, and unbind the kernel driver \
         (e.g. ftdi_sio) if it is bound.",
    ];

    if error.to_ascii_lowercase().contains("claim") {
        CLAIM_FAILED
    } else {
        OTHER
    }
}

fn ui_option_field(ui: &mut egui::Ui, field: &mut Field) {
    let spec = field.spec;
    ui.label(spec.label).on_hover_text(spec.help);
    match spec.kind {
        OptionKind::Int { .. } => {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut field.value)
                        .desired_width(80.0)
                        .hint_text(UNSET_LABEL),
                )
                .on_hover_text(spec.help);
                if let Some(err) = int_error(spec, &field.value) {
                    ui.colored_label(egui::Color32::from_rgb(220, 60, 60), err);
                }
            });
        }
        OptionKind::Choice(choices) => {
            let current = choices
                .iter()
                .find(|c| c.value == field.value)
                .map(|c| c.label)
                .unwrap_or(UNSET_LABEL);
            egui::ComboBox::from_id_salt(spec.key)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    if spec.default.is_none() {
                        ui.selectable_value(&mut field.value, String::new(), UNSET_LABEL);
                    }
                    for c in choices {
                        ui.selectable_value(&mut field.value, c.value.to_string(), c.label);
                    }
                })
                .response
                .on_hover_text(spec.help);
        }
        OptionKind::SpeedKhz { presets } => {
            ui.horizontal(|ui| {
                if !presets.is_empty() {
                    let current = presets
                        .iter()
                        .find(|p| p.to_string() == field.value)
                        .map(|&p| speed_label(p))
                        .unwrap_or_else(|| {
                            if field.value.is_empty() {
                                UNSET_LABEL.to_string()
                            } else {
                                "custom".to_string()
                            }
                        });
                    egui::ComboBox::from_id_salt(spec.key)
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            if spec.default.is_none() {
                                ui.selectable_value(&mut field.value, String::new(), UNSET_LABEL);
                            }
                            for &p in presets {
                                ui.selectable_value(
                                    &mut field.value,
                                    p.to_string(),
                                    speed_label(p),
                                );
                            }
                        });
                }
                ui.add(
                    egui::TextEdit::singleline(&mut field.value)
                        .desired_width(70.0)
                        .hint_text("kHz"),
                )
                .on_hover_text("Plain kHz or a k/m/g suffix, e.g. 30m");
            });
        }
        OptionKind::Text => {
            ui.add(
                egui::TextEdit::singleline(&mut field.value)
                    .desired_width(140.0)
                    .hint_text(UNSET_LABEL),
            )
            .on_hover_text(spec.help);
        }
        // Filtered out by `ProgrammerForm::new`; nothing sensible to show.
        OptionKind::Path => {}
    }
    ui.end_row();
}

impl eframe::App for RflasherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Store context for async tasks to request repaints
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }

        // Process messages from async tasks
        self.process_messages();

        // Request repaint while operations are in progress
        if self.is_busy() || matches!(self.connection, ConnectionState::Connecting) {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("rflasher");
                ui.separator();
                ui.label("Flash Programmer");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("Help", |ui| {
                        if ui.button("USB permissions (udev rules)").clicked() {
                            self.show_udev_window = true;
                            ui.close();
                        }
                    });
                });
            });
        });

        // Udev rules popup window
        if self.show_udev_window {
            self.ui_udev_window(ctx);
        }

        egui::SidePanel::left("controls")
            .min_width(250.0)
            .show(ctx, |ui| {
                self.ui_connection(ui);
                ui.separator();
                self.ui_operations(ui);
                ui.separator();
                self.ui_file_ops(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_status(ui);
        });
    }
}

impl RflasherApp {
    fn ui_connection(&mut self, ui: &mut egui::Ui) {
        ui.heading("Connection");
        ui.add_space(5.0);

        let connected = self.is_connected();

        // Programmer picker, driven by the shared catalog.
        let mut picked: Option<usize> = None;
        ui.add_enabled_ui(!connected, |ui| {
            ui.horizontal(|ui| {
                ui.label("Programmer:");
                egui::ComboBox::from_id_salt("programmer")
                    .selected_text(self.form.name)
                    .show_ui(ui, |ui| {
                        for (i, info) in self.programmers.iter().enumerate() {
                            let selected = info.name == self.form.name;
                            if ui
                                .selectable_label(selected, info.name)
                                .on_hover_text(info.description)
                                .clicked()
                                && !selected
                            {
                                picked = Some(i);
                            }
                        }
                    });
            });
        });
        if let Some(i) = picked {
            self.form = ProgrammerForm::new(&self.programmers[i]);
        }

        // Option widgets generated from the schema. Borrow the catalog entry
        // directly so the form fields can be edited alongside it.
        let info = self.programmers.iter().find(|p| p.name == self.form.name);
        let validation = match info {
            Some(info) => {
                if !info.description.is_empty() {
                    ui.small(info.description);
                }
                let has_fields = !self.form.fields.is_empty();
                ui.add_enabled_ui(!connected, |ui| {
                    egui::Grid::new("programmer_options")
                        .num_columns(2)
                        .show(ui, |ui| {
                            for field in &mut self.form.fields {
                                ui_option_field(ui, field);
                            }
                        });
                });
                if connected && has_fields {
                    ui.small("Reconnect to change options");
                }
                self.form.validate(info)
            }
            None => Err("No programmer selected".to_string()),
        };
        if let Err(ref e) = validation {
            ui.colored_label(egui::Color32::from_rgb(220, 60, 60), e);
        }

        ui.add_space(5.0);

        // Connection status and button
        match &self.connection {
            ConnectionState::Disconnected => {
                let button_label = if self.selected_is_webusb() {
                    "Connect (WebUSB)"
                } else {
                    "Connect (WebSerial)"
                };
                ui.add_enabled_ui(validation.is_ok(), |ui| {
                    if ui.button(button_label).clicked() {
                        self.spawn_connect();
                    }
                });
            }
            ConnectionState::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Connecting...");
                });
            }
            ConnectionState::Connected { programmer_name } => {
                ui.colored_label(egui::Color32::GREEN, "Connected");
                ui.label(format!("Programmer: {}", programmer_name));
                if let Some(ref info) = self.chip_info {
                    ui.label(format!("Chip: {}", info.chip.name));
                    ui.label(format!("Size: {} KB", info.size / 1024));
                }
                ui.add_space(5.0);
                if ui.button("Disconnect").clicked() {
                    self.spawn_disconnect();
                }
            }
        }
    }

    fn ui_operations(&mut self, ui: &mut egui::Ui) {
        ui.heading("Operations");
        ui.add_space(5.0);

        let connected = self.is_connected();
        let busy = self.is_busy();
        let has_chip = self.chip_detected();

        // Probe button
        ui.add_enabled_ui(connected && !busy, |ui| {
            if ui.button("Probe Chip").clicked() {
                self.spawn_probe();
            }
        });

        ui.add_space(5.0);

        // Read/Write/Erase/Verify buttons
        ui.add_enabled_ui(connected && has_chip && !busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Read").clicked() {
                    self.spawn_read();
                }
                if ui.button("Write").clicked() {
                    self.spawn_write();
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Erase").clicked() {
                    self.spawn_erase();
                }
                if ui.button("Verify").clicked() {
                    self.spawn_verify();
                }
            });
        });

        // Progress display
        match &self.operation {
            OperationState::Reading {
                bytes_done,
                bytes_total,
            } => {
                let progress = *bytes_done as f32 / *bytes_total as f32;
                ui.add_space(5.0);
                ui.label("Reading...");
                ui.add(egui::ProgressBar::new(progress).show_percentage());
                ui.label(format!("{} / {} bytes", bytes_done, bytes_total));
            }
            OperationState::Verifying {
                bytes_done,
                bytes_total,
            } => {
                let progress = *bytes_done as f32 / *bytes_total as f32;
                ui.add_space(5.0);
                ui.label("Verifying...");
                ui.add(egui::ProgressBar::new(progress).show_percentage());
                ui.label(format!("{} / {} bytes", bytes_done, bytes_total));
            }
            OperationState::Writing {
                bytes_done,
                bytes_total,
                phase,
            } => {
                let progress = *bytes_done as f32 / *bytes_total as f32;
                let phase_str = match phase {
                    WritePhase::Reading => "Reading current contents...",
                    WritePhase::Erasing => "Erasing...",
                    WritePhase::Writing => "Writing...",
                };
                ui.add_space(5.0);
                ui.label(phase_str);
                ui.add(egui::ProgressBar::new(progress).show_percentage());
                ui.label(format!("{} / {} bytes", bytes_done, bytes_total));
            }
            OperationState::Erasing {
                bytes_done,
                bytes_total,
            } => {
                let progress = *bytes_done as f32 / *bytes_total as f32;
                ui.add_space(5.0);
                ui.label("Erasing...");
                ui.add(egui::ProgressBar::new(progress).show_percentage());
                ui.label(format!("{} / {} bytes", bytes_done, bytes_total));
            }
            OperationState::Probing => {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Probing...");
                });
            }
            OperationState::Idle => {}
        }
    }

    fn ui_file_ops(&mut self, ui: &mut egui::Ui) {
        ui.heading("File");
        ui.add_space(5.0);

        // File status
        if let Some(ref buf) = self.file_buffer {
            ui.label(format!("Loaded: {} bytes", buf.len()));
            if ui.button("Clear").clicked() {
                self.file_buffer = None;
                self.status.info("File buffer cleared");
            }
        } else {
            ui.label("No file loaded");
        }

        ui.add_space(5.0);

        // Load file button - will be implemented with file dialog
        if ui.button("Load File...").clicked() {
            self.spawn_file_load();
        }

        // Save file button (only if we have data)
        ui.add_enabled_ui(self.file_buffer.is_some(), |ui| {
            if ui.button("Save File...").clicked() {
                self.spawn_file_save();
            }
        });
    }

    fn spawn_file_load(&mut self) {
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();

        self.status.info("Opening file dialog...");

        wasm_bindgen_futures::spawn_local(async move {
            match load_file_dialog().await {
                Ok(data) => {
                    let size = data.len();
                    shared.borrow_mut().messages.push(AsyncMessage::Log(
                        LogLevel::Success,
                        format!("Loaded {} bytes", size),
                    ));
                    shared
                        .borrow_mut()
                        .messages
                        .push(AsyncMessage::ReadComplete(data));
                }
                Err(e) => {
                    shared.borrow_mut().messages.push(AsyncMessage::Log(
                        LogLevel::Error,
                        format!("Failed to load file: {}", e),
                    ));
                }
            }

            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_file_save(&mut self) {
        let Some(ref data) = self.file_buffer else {
            return;
        };

        let data = data.clone();
        self.status.info("Saving file...");

        wasm_bindgen_futures::spawn_local(async move {
            match save_file_dialog(&data, "flash_dump.bin").await {
                Ok(()) => {
                    log::info!("File saved");
                }
                Err(e) => {
                    log::error!("Failed to save file: {}", e);
                }
            }
        });
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status Log");
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (level, msg) in &self.status.messages {
                    match level {
                        LogLevel::Info => {
                            // Use default text color for info messages
                            ui.label(msg);
                        }
                        LogLevel::Success => {
                            ui.colored_label(egui::Color32::from_rgb(0, 180, 0), msg);
                        }
                        LogLevel::Warning => {
                            ui.colored_label(egui::Color32::from_rgb(220, 180, 0), msg);
                        }
                        LogLevel::Error => {
                            ui.colored_label(egui::Color32::from_rgb(220, 60, 60), msg);
                        }
                    };
                }
            });
    }

    fn ui_udev_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("USB Permissions (Linux udev rules)")
            .open(&mut self.show_udev_window)
            .min_width(520.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(
                    "On Linux, WebUSB requires udev rules to grant browser access to USB devices.",
                );
                ui.label(
                    "Create the file below and replug the device (or run the reload commands).",
                );
                ui.label(
                    "Raiden Debug SPI / Cr50 uses Google VID 18d1. Product IDs vary between SuzyQ, Servo, C2D2, uServo, and Servo Micro, so the example rule below matches Google debug hardware broadly.",
                );
                ui.label(
                    "These rules only grant access. A kernel driver can still hold \
                     the interface, and WebUSB cannot detach it: if connecting fails \
                     with \"Unable to claim interface\", unbind the driver first \
                     (for FTDI: `echo -n \"1-4\" | sudo tee /sys/bus/usb/drivers/ftdi_sio/unbind`).",
                );
                ui.add_space(5.0);

                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.monospace("# /etc/udev/rules.d/50-rflasher.rules");
                    ui.add_space(3.0);
                    ui.monospace("# CH341A (VID:1a86 PID:5512)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"1a86\", ",
                        "ATTR{idProduct}==\"5512\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace("# CH347T (VID:1a86 PID:55db)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"1a86\", ",
                        "ATTR{idProduct}==\"55db\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace("# CH347F (VID:1a86 PID:55de)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"1a86\", ",
                        "ATTR{idProduct}==\"55de\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace("# FTDI FT2232H (VID:0403 PID:6010)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"0403\", ",
                        "ATTR{idProduct}==\"6010\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace("# FTDI FT4232H (VID:0403 PID:6011)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"0403\", ",
                        "ATTR{idProduct}==\"6011\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace("# FTDI FT232H (VID:0403 PID:6014)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"0403\", ",
                        "ATTR{idProduct}==\"6014\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace("# FTDI FT4222H (VID:0403 PID:601c)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"0403\", ",
                        "ATTR{idProduct}==\"601c\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace("# Dediprog SF100/SF200/SF600/SF700 (VID:0483 PID:dada)");
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"0483\", ",
                        "ATTR{idProduct}==\"dada\", MODE=\"0666\"",
                    ));
                    ui.add_space(3.0);
                    ui.monospace(
                        "# Raiden Debug SPI / Cr50 (Google debug hardware, VID:18d1; product ID varies)",
                    );
                    ui.monospace(concat!(
                        "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"18d1\", ",
                        "MODE=\"0666\"",
                    ));
                });

                ui.add_space(5.0);
                ui.label("Then reload udev rules:");
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.monospace("sudo udevadm control --reload-rules");
                    ui.monospace("sudo udevadm trigger");
                });
            });
    }
}

// =============================================================================
// File I/O using browser APIs
// =============================================================================

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// Load a file using the browser's file input dialog
async fn load_file_dialog() -> Result<Vec<u8>, String> {
    use web_sys::{Document, HtmlInputElement};

    let window = web_sys::window().ok_or("No window")?;
    let document: Document = window.document().ok_or("No document")?;

    // Create a hidden file input
    let input: HtmlInputElement = document
        .create_element("input")
        .map_err(|_| "Failed to create input")?
        .dyn_into()
        .map_err(|_| "Not an input element")?;

    input.set_type("file");
    input.set_accept(".bin,.rom,.img,*/*");

    // Use a promise to wait for the file selection
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<u8>, String>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let closure = {
        let input = input.clone();
        let tx = tx.clone();

        Closure::once(Box::new(move || {
            let files = input.files();
            if let Some(files) = files
                && files.length() > 0
                && let Some(file) = files.get(0)
            {
                let tx = tx.clone();
                let reader = web_sys::FileReader::new().unwrap();
                let reader_clone = reader.clone();

                let onload = Closure::once(Box::new(move || {
                    let result = reader_clone.result().unwrap();
                    let array = js_sys::Uint8Array::new(&result);
                    let data = array.to_vec();
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(Ok(data));
                    }
                }) as Box<dyn FnOnce()>);

                reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                onload.forget();

                reader.read_as_array_buffer(&file).unwrap();
                return;
            }
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Err("No file selected".to_string()));
            }
        }) as Box<dyn FnOnce()>)
    };

    input.set_onchange(Some(closure.as_ref().unchecked_ref()));
    closure.forget();

    // Click the input to open dialog
    input.click();

    // Wait for result
    rx.await.map_err(|_| "Channel closed".to_string())?
}

/// Save a file using the browser's download functionality
async fn save_file_dialog(data: &[u8], filename: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("No window")?;
    let document = window.document().ok_or("No document")?;

    // Create a Blob from the data
    let array = js_sys::Uint8Array::from(data);
    let blob_parts = js_sys::Array::new();
    blob_parts.push(&array);

    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/octet-stream");

    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&blob_parts, &options)
        .map_err(|_| "Failed to create blob")?;

    // Create a URL for the blob
    let url =
        web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| "Failed to create URL")?;

    // Create a download link and click it
    let a: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|_| "Failed to create anchor")?
        .dyn_into()
        .map_err(|_| "Not an anchor element")?;

    a.set_href(&url);
    a.set_download(filename);
    a.click();

    // Clean up the URL
    web_sys::Url::revoke_object_url(&url).map_err(|_| "Failed to revoke URL")?;

    Ok(())
}

#[cfg(test)]
mod connection_hint_tests {
    use super::webusb_failure_hints;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn claim_failures_get_the_kernel_driver_hint() {
        let hints = webusb_failure_hints(
            "Failed to execute 'claimInterface' on 'USBDevice': Unable to claim interface.",
        );
        assert!(hints.iter().any(|h| h.contains("ftdi_sio/unbind")));
        assert!(hints.iter().any(|h| h.contains("blacklist")));
    }

    #[wasm_bindgen_test]
    fn other_failures_get_the_generic_hint() {
        let hints = webusb_failure_hints("Device not found");
        assert!(hints.iter().any(|h| h.contains("udev")));
        // The specific unbind-by-bus-port instructions are reserved for claim failures.
        assert!(!hints.iter().any(|h| h.contains("lsusb")));
    }
}
