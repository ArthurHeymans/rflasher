//! Main egui application for rflasher web interface

use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use rflasher_ch341a::Ch341a;
use rflasher_ch347::{Ch347, SpiSpeed};
use rflasher_core::chip::{ChipDatabase, FlashChip};
use rflasher_core::flash::unified::{smart_write, WriteProgress, WriteStats};
use rflasher_core::flash::{FlashContext, FlashDevice, ProbeResult, SpiFlashDevice};
use rflasher_serprog::Serprog;

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
// Programmer type abstraction
// =============================================================================

/// The type of programmer to connect to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgrammerType {
    /// serprog via WebSerial
    Serprog,
    /// CH341A via WebUSB
    Ch341a,
    /// CH347 via WebUSB
    Ch347,
}

impl ProgrammerType {
    fn label(&self) -> &'static str {
        match self {
            ProgrammerType::Serprog => "serprog (WebSerial)",
            ProgrammerType::Ch341a => "CH341A (WebUSB)",
            ProgrammerType::Ch347 => "CH347 (WebUSB)",
        }
    }

    /// Whether this programmer uses WebUSB (vs WebSerial)
    fn is_webusb(&self) -> bool {
        matches!(self, ProgrammerType::Ch341a | ProgrammerType::Ch347)
    }
}

/// Connected programmer - wraps a serprog, CH341A, or CH347 device
enum Programmer {
    Serprog(Serprog<WebSerialTransport>),
    Ch341a(Ch341a),
    Ch347(Ch347),
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
        }
    };
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
    /// Baud rate for serial connection
    baud_rate: u32,
    /// Selected programmer type
    programmer_type: ProgrammerType,
    /// Selected SPI speed for CH347
    spi_speed: SpiSpeed,
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
    messages: Vec<(LogLevel, String)>,
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
            messages: Vec::new(),
            max_messages: 100,
        }
    }
}

impl StatusLog {
    fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.messages.push((level, message.into()));
        if self.messages.len() > self.max_messages {
            self.messages.remove(0);
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
        Self {
            shared: Rc::new(RefCell::new(SharedState::default())),
            connection: ConnectionState::Disconnected,
            operation: OperationState::Idle,
            status: StatusLog::default(),
            file_buffer: None,
            baud_rate: 115200,
            programmer_type: ProgrammerType::Serprog,
            spi_speed: SpiSpeed::default(),
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
                    if self.programmer_type.is_webusb() {
                        self.status.warn(
                            "On Linux, this may be a permissions issue. \
                             Check Help > USB permissions for udev rules.",
                        );
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
        match self.programmer_type {
            ProgrammerType::Serprog => self.spawn_connect_serprog(),
            ProgrammerType::Ch341a => self.spawn_connect_ch341a(),
            ProgrammerType::Ch347 => self.spawn_connect_ch347(),
        }
    }

    fn spawn_connect_serprog(&mut self) {
        let baud_rate = self.baud_rate;
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();

        self.connection = ConnectionState::Connecting;
        self.status.info("Requesting serial port...");

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            match WebSerialTransport::request_and_open(baud_rate).await {
                Ok(transport) => {
                    // Create serprog device
                    match Serprog::new(transport).await {
                        Ok(serprog) => {
                            let name = serprog.info().name_str().to_string();
                            {
                                let mut state = shared.borrow_mut();
                                state.programmer = Some(Programmer::Serprog(serprog));
                                state.messages.push(AsyncMessage::Connected {
                                    programmer_name: name,
                                });
                            }
                        }
                        Err(e) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::ConnectionFailed(format!("{:?}", e)));
                        }
                    }
                }
                Err(e) => {
                    shared
                        .borrow_mut()
                        .messages
                        .push(AsyncMessage::ConnectionFailed(format!("{:?}", e)));
                }
            }

            shared.borrow_mut().busy = false;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_connect_ch341a(&mut self) {
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();

        self.connection = ConnectionState::Connecting;
        self.status.info("Requesting CH341A device via WebUSB...");

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            match Ch341a::request_device().await {
                Ok(device_info) => match Ch341a::open(device_info).await {
                    Ok(ch341a) => {
                        let mut state = shared.borrow_mut();
                        state.programmer = Some(Programmer::Ch341a(ch341a));
                        state.messages.push(AsyncMessage::Connected {
                            programmer_name: "CH341A".to_string(),
                        });
                    }
                    Err(e) => {
                        shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::ConnectionFailed(format!("{}", e)));
                    }
                },
                Err(e) => {
                    shared
                        .borrow_mut()
                        .messages
                        .push(AsyncMessage::ConnectionFailed(format!("{}", e)));
                }
            }

            shared.borrow_mut().busy = false;
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn spawn_connect_ch347(&mut self) {
        let shared = self.shared.clone();
        let ctx = self.ctx.clone();
        let spi_speed = self.spi_speed;

        self.connection = ConnectionState::Connecting;
        self.status.info("Requesting CH347 device via WebUSB...");

        wasm_bindgen_futures::spawn_local(async move {
            shared.borrow_mut().busy = true;

            let config = rflasher_ch347::SpiConfig::default().with_speed(spi_speed);

            match Ch347::request_device().await {
                Ok(device_info) => match Ch347::open_with_config(device_info, config).await {
                    Ok(ch347) => {
                        let variant_name = match ch347.variant() {
                            rflasher_ch347::Ch347Variant::Ch347T => "CH347T",
                            rflasher_ch347::Ch347Variant::Ch347F => "CH347F",
                        };
                        let mut state = shared.borrow_mut();
                        state.programmer = Some(Programmer::Ch347(ch347));
                        state.messages.push(AsyncMessage::Connected {
                            programmer_name: variant_name.to_string(),
                        });
                    }
                    Err(e) => {
                        shared
                            .borrow_mut()
                            .messages
                            .push(AsyncMessage::ConnectionFailed(format!("{}", e)));
                    }
                },
                Err(e) => {
                    shared
                        .borrow_mut()
                        .messages
                        .push(AsyncMessage::ConnectionFailed(format!("{}", e)));
                }
            }

            shared.borrow_mut().busy = false;
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
                use rflasher_core::flash::unified::read_with_progress;

                let ctx_flash = FlashContext::new(chip);
                let mut buf = vec![0u8; size];
                let mut progress = SharedProgress::new(shared.clone(), ctx.clone());

                with_programmer!(shared, programmer, master, {
                    let mut device = SpiFlashDevice::new(master, ctx_flash);
                    let result = read_with_progress(&mut device, &mut buf, &mut progress).await;
                    let (m, _) = device.into_parts();
                    master = m;

                    match result {
                        Ok(()) => {
                            shared
                                .borrow_mut()
                                .messages
                                .push(AsyncMessage::ReadComplete(buf));
                        }
                        Err(e) => {
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

                with_programmer!(shared, programmer, master, {
                    let mut device = SpiFlashDevice::new(master, ctx_flash);
                    let result = smart_write(&mut device, &data, &mut progress).await;
                    let (m, _) = device.into_parts();
                    master = m;

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

                with_programmer!(shared, programmer, master, {
                    let mut device = SpiFlashDevice::new(master, ctx_flash);
                    let result = device.erase(0, size).await;
                    let (m, _) = device.into_parts();
                    master = m;

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

                // Helper closure-like async block for verification
                async fn do_verify<M: rflasher_core::programmer::SpiMaster>(
                    master: M,
                    ctx_flash: FlashContext,
                    data: &[u8],
                    shared: &SharedStateRef,
                    ctx: &Option<egui::Context>,
                ) -> (M, Option<String>) {
                    let mut device = SpiFlashDevice::new(master, ctx_flash);

                    const CHUNK_SIZE: usize = 4096;
                    const YIELD_INTERVAL: usize = 65536;
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
                                                offset + i, a, b
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

                    let (master, _) = device.into_parts();
                    (master, verify_error)
                }

                with_programmer!(shared, programmer, master, {
                    let (m, verify_error) =
                        do_verify(master, ctx_flash, &data, &shared, &ctx).await;
                    master = m;

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

        // Programmer type selection
        ui.horizontal(|ui| {
            ui.label("Programmer:");
            egui::ComboBox::from_id_salt("programmer_type")
                .selected_text(self.programmer_type.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.programmer_type,
                        ProgrammerType::Serprog,
                        ProgrammerType::Serprog.label(),
                    );
                    ui.selectable_value(
                        &mut self.programmer_type,
                        ProgrammerType::Ch341a,
                        ProgrammerType::Ch341a.label(),
                    );
                    ui.selectable_value(
                        &mut self.programmer_type,
                        ProgrammerType::Ch347,
                        ProgrammerType::Ch347.label(),
                    );
                });
        });

        // Baud rate selection (only for serprog)
        if self.programmer_type == ProgrammerType::Serprog {
            ui.horizontal(|ui| {
                ui.label("Baud rate:");
                egui::ComboBox::from_id_salt("baud_rate")
                    .selected_text(format!("{}", self.baud_rate))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.baud_rate, 9600, "9600");
                        ui.selectable_value(&mut self.baud_rate, 19200, "19200");
                        ui.selectable_value(&mut self.baud_rate, 38400, "38400");
                        ui.selectable_value(&mut self.baud_rate, 57600, "57600");
                        ui.selectable_value(&mut self.baud_rate, 115200, "115200");
                        ui.selectable_value(&mut self.baud_rate, 230400, "230400");
                        ui.selectable_value(&mut self.baud_rate, 460800, "460800");
                        ui.selectable_value(&mut self.baud_rate, 921600, "921600");
                        ui.selectable_value(&mut self.baud_rate, 1000000, "1000000");
                        ui.selectable_value(&mut self.baud_rate, 2000000, "2000000");
                    });
            });
        }

        // SPI speed selection (only for CH347)
        if self.programmer_type == ProgrammerType::Ch347 {
            let connected = self.is_connected();
            ui.add_enabled_ui(!connected, |ui| {
                ui.horizontal(|ui| {
                    ui.label("SPI speed:");
                    egui::ComboBox::from_id_salt("spi_speed")
                        .selected_text(self.spi_speed.label())
                        .show_ui(ui, |ui| {
                            for &speed in SpiSpeed::ALL {
                                ui.selectable_value(&mut self.spi_speed, speed, speed.label());
                            }
                        });
                });
            });
            if connected {
                ui.label("Reconnect to change SPI speed");
            }
        }

        ui.add_space(5.0);

        // Connection status and button
        match &self.connection {
            ConnectionState::Disconnected => {
                let button_label = if self.programmer_type.is_webusb() {
                    "Connect (WebUSB)"
                } else {
                    "Connect (WebSerial)"
                };
                if ui.button(button_label).clicked() {
                    self.spawn_connect();
                }
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

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

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
            if let Some(files) = files {
                if files.length() > 0 {
                    if let Some(file) = files.get(0) {
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
                }
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
