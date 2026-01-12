//! Main egui application for rflasher web interface

use eframe::egui;

use rflasher_core::chip::{ChipDatabase, FlashChip};
use rflasher_core::flash::ProbeResult;

/// Application state
pub struct RflasherApp {
    /// Current connection state
    connection: ConnectionState,
    /// Operation state (what we're currently doing)
    operation: OperationState,
    /// Status messages
    status: StatusLog,
    /// File data for read/write operations
    file_buffer: Option<Vec<u8>>,
    /// Baud rate for serial connection
    baud_rate: u32,
    /// Chip database
    chip_db: ChipDatabase,
    /// Last probe result
    probe_result: Option<ProbeResult>,
}

/// Connection state
#[derive(Default)]
enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected(ConnectionInfo),
}

/// Information about an active connection
struct ConnectionInfo {
    programmer_name: String,
    chip: Option<FlashChip>,
    chip_size: Option<u32>,
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
    Erasing,
    Verifying {
        bytes_done: usize,
        bytes_total: usize,
    },
    #[allow(dead_code)]
    Error(String),
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

#[derive(Clone, Copy)]
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

    #[allow(dead_code)]
    fn success(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Success, message);
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Warning, message);
    }

    #[allow(dead_code)]
    fn error(&mut self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }
}

impl Default for RflasherApp {
    fn default() -> Self {
        Self {
            connection: ConnectionState::Disconnected,
            operation: OperationState::Idle,
            status: StatusLog::default(),
            file_buffer: None,
            baud_rate: 115200,
            chip_db: ChipDatabase::new(),
            probe_result: None,
        }
    }
}

impl RflasherApp {
    /// Create a new application
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    fn is_busy(&self) -> bool {
        !matches!(
            self.operation,
            OperationState::Idle | OperationState::Error(_)
        )
    }

    fn is_connected(&self) -> bool {
        matches!(self.connection, ConnectionState::Connected(_))
    }

    fn chip_detected(&self) -> bool {
        if let ConnectionState::Connected(ref info) = self.connection {
            info.chip.is_some()
        } else {
            false
        }
    }
}

impl eframe::App for RflasherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint while operations are in progress
        if self.is_busy() || matches!(self.connection, ConnectionState::Connecting) {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("rflasher");
                ui.separator();
                ui.label("Flash Programmer");
            });
        });

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

        // Baud rate selection
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

        ui.add_space(5.0);

        // Connection status and button
        match &self.connection {
            ConnectionState::Disconnected => {
                if ui.button("Connect (WebSerial)").clicked() {
                    self.status.info("Requesting serial port...");
                    self.connection = ConnectionState::Connecting;
                    // TODO: Spawn async connect task
                }
            }
            ConnectionState::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Connecting...");
                });
            }
            ConnectionState::Connected(info) => {
                ui.colored_label(egui::Color32::GREEN, "Connected");
                ui.label(format!("Programmer: {}", info.programmer_name));
                if let Some(ref chip) = info.chip {
                    ui.label(format!("Chip: {}", chip.name));
                    if let Some(size) = info.chip_size {
                        ui.label(format!("Size: {} KB", size / 1024));
                    }
                }
                ui.add_space(5.0);
                if ui.button("Disconnect").clicked() {
                    self.connection = ConnectionState::Disconnected;
                    self.probe_result = None;
                    self.status.info("Disconnected");
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
                self.operation = OperationState::Probing;
                self.status.info("Probing chip...");
                // TODO: Spawn async probe task
            }
        });

        ui.add_space(5.0);

        // Read/Write/Erase/Verify buttons
        ui.add_enabled_ui(connected && has_chip && !busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Read").clicked() {
                    self.status.info("Starting read operation...");
                    // TODO: Start read operation
                }
                if ui.button("Write").clicked() {
                    if self.file_buffer.is_some() {
                        self.status.info("Starting write operation...");
                        // TODO: Start write operation
                    } else {
                        self.status.warn("No file loaded to write");
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Erase").clicked() {
                    self.status.info("Starting erase operation...");
                    // TODO: Start erase operation
                }
                if ui.button("Verify").clicked() {
                    if self.file_buffer.is_some() {
                        self.status.info("Starting verify operation...");
                        // TODO: Start verify operation
                    } else {
                        self.status.warn("No file loaded to verify");
                    }
                }
            });
        });

        // Progress display
        if let OperationState::Reading {
            bytes_done,
            bytes_total,
        }
        | OperationState::Verifying {
            bytes_done,
            bytes_total,
        } = &self.operation
        {
            let progress = *bytes_done as f32 / *bytes_total as f32;
            ui.add_space(5.0);
            ui.add(egui::ProgressBar::new(progress).show_percentage());
            ui.label(format!("{} / {} bytes", bytes_done, bytes_total));
        }

        if let OperationState::Writing {
            bytes_done,
            bytes_total,
            phase,
        } = &self.operation
        {
            let progress = *bytes_done as f32 / *bytes_total as f32;
            let phase_str = match phase {
                WritePhase::Reading => "Reading current",
                WritePhase::Erasing => "Erasing",
                WritePhase::Writing => "Writing",
            };
            ui.add_space(5.0);
            ui.label(phase_str);
            ui.add(egui::ProgressBar::new(progress).show_percentage());
            ui.label(format!("{} / {} bytes", bytes_done, bytes_total));
        }

        if let OperationState::Probing = &self.operation {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Probing...");
            });
        }

        if let OperationState::Erasing = &self.operation {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Erasing...");
            });
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

        // Load file button
        if ui.button("Load File...").clicked() {
            // Use native file dialog via rfd or manual implementation
            self.status
                .info("File loading via dialog not yet implemented");
            // TODO: Implement file loading
        }

        // Save file button (only if we have data)
        ui.add_enabled_ui(self.file_buffer.is_some(), |ui| {
            if ui.button("Save File...").clicked() {
                // TODO: Implement file saving
                self.status
                    .info("File saving via dialog not yet implemented");
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
                    let color = match level {
                        LogLevel::Info => egui::Color32::WHITE,
                        LogLevel::Success => egui::Color32::GREEN,
                        LogLevel::Warning => egui::Color32::YELLOW,
                        LogLevel::Error => egui::Color32::RED,
                    };
                    ui.colored_label(color, msg);
                }
            });
    }
}
