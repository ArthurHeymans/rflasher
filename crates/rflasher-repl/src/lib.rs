//! Steel Scheme REPL for scripting raw SPI commands
//!
//! This crate provides a Scheme REPL (Read-Eval-Print Loop) that exposes
//! raw SPI commands for flash chip programming. It's designed for advanced
//! users who need to script custom SPI sequences.
//!
//! # Features
//!
//! - Raw SPI command execution (single, dual, quad I/O modes)
//! - All standard SPI25 opcodes as constants
//! - Helper functions for common operations (read-jedec-id, read-status, etc.)
//! - Byte vector operations for data manipulation
//! - Syntax highlighting and bracket matching
//! - Command history with arrow key navigation
//! - Tab completion for known functions
//!
//! # Example Session
//!
//! ```scheme
//! λ > (read-jedec-id)
//! => (239 16385)  ; manufacturer=0xEF, device=0x4014
//!
//! λ > (read-status1)
//! => 0
//!
//! λ > (spi-read READ 0 16)
//! => (255 255 255 255 255 255 255 255 255 255 255 255 255 255 255 255)
//!
//! λ > (define data (make-bytes 256 #xAA))
//! λ > (spi-write PP #x1000 data)
//! => #t
//! ```

mod error;
pub mod highlight;
mod spi_module;

pub use error::ReplError;
pub use spi_module::{create_constants_module, create_spi_module};

use crate::highlight::ReplHelper;
use colored::Colorize;
use directories::ProjectDirs;
use rflasher_core::programmer::SpiMaster;
use rustyline::config::Configurer;
use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use rustyline::Editor;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;
use steel_parser::interner::InternedString;

/// Boxed SPI master type for dynamic dispatch
pub type BoxedSpiMaster = Box<dyn SpiMaster + Send>;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Get the ASCII art banner
fn get_banner() -> String {
    format!(
        r#"
        __ _           _
   _ _ / _| |__ _ _____| |_  ___ _ _
  | '_|  _| / _` (_-< ' \/ -_) '_|    Version {}
  |_| |_| |_\__,_/__/_||_\___|_|      :? for help
"#,
        VERSION
    )
    .bright_yellow()
    .bold()
    .to_string()
}

/// Get the history file path
fn get_history_path() -> PathBuf {
    if let Some(proj_dirs) = ProjectDirs::from("", "", "rflasher") {
        let mut path = proj_dirs.data_dir().to_path_buf();
        std::fs::create_dir_all(&path).ok();
        path.push("repl_history");
        path
    } else {
        PathBuf::from(".rflasher_history")
    }
}

/// Collect global identifiers from the engine for completion/highlighting
fn collect_globals(_engine: &Engine) -> HashSet<InternedString> {
    let mut globals = HashSet::new();

    // Add our known rflasher functions
    let known_functions = [
        // SPI operations
        "spi-transfer",
        "spi-read",
        "spi-write",
        "read-jedec-id",
        "read-status1",
        "read-status2",
        "read-status3",
        "write-enable",
        "write-disable",
        "is-busy?",
        "wait-ready",
        "chip-erase",
        "sector-erase",
        "block-erase-32k",
        "block-erase-64k",
        // Byte utilities
        "make-bytes",
        "bytes-length",
        "bytes-ref",
        "bytes-set!",
        "bytes->list",
        "list->bytes",
        "bytes->hex",
        "hex->bytes",
        "bytes-slice",
        // Help
        "rflasher-help",
        // SPI25 constants
        "WREN",
        "WRDI",
        "RDSR",
        "WRSR",
        "READ",
        "FAST_READ",
        "PP",
        "SE",
        "BE_32K",
        "BE_64K",
        "CE",
        "RDID",
        "RDSFDP",
        "RDSR2",
        "RDSR3",
        "WRSR2",
        "WRSR3",
        "SR1_WIP",
        "SR1_WEL",
        "SR1_BP0",
        "SR1_BP1",
        "SR1_BP2",
        "SR1_TB",
        "SR1_SEC",
        "SR1_SRP0",
    ];

    for name in known_functions {
        globals.insert(InternedString::try_get(name).unwrap_or_else(|| InternedString::from(name)));
    }

    // Try to get globals from engine (this may not be available in all steel versions)
    // For now we rely on the known functions list

    globals
}

/// Run the Steel REPL with a boxed SPI master
pub fn run_repl_boxed(master: BoxedSpiMaster) -> Result<(), ReplError> {
    let mut engine = Engine::new();

    // Wrap master in Arc<Mutex> for thread-safe access from Steel
    let master = Arc::new(Mutex::new(master));

    // Register the SPI module
    let module = spi_module::create_spi_module_boxed(Arc::clone(&master));
    engine.register_module(module);

    // Register the SPI25 constants module
    let constants_module = spi_module::create_constants_module();
    engine.register_module(constants_module);

    // Register the prelude that requires the modules
    engine
        .run(
            r#"
        (require-builtin rflasher/spi)
        (require-builtin rflasher/spi25)
    "#,
        )
        .map_err(|e| ReplError::SteelError(format!("{}", e)))?;

    // Collect globals for completion/highlighting
    let globals = Arc::new(Mutex::new(collect_globals(&engine)));

    // Create rustyline editor with helper
    let helper = ReplHelper::new(Arc::clone(&globals));
    let mut rl = Editor::<ReplHelper, FileHistory>::new()
        .map_err(|e| ReplError::IoError(std::io::Error::other(e)))?;
    rl.set_helper(Some(helper));
    rl.set_check_cursor_position(true);

    // Load history
    let history_path = get_history_path();
    if rl.load_history(&history_path).is_err() {
        // History file doesn't exist yet, that's fine
    }

    // Print banner
    println!("{}", get_banner());
    println!(
        "Type {} for available commands, {} or {} to exit.",
        "(rflasher-help)".bright_cyan(),
        "(quit)".bright_cyan(),
        "(exit)".bright_cyan()
    );
    println!();

    // Create the prompt
    let prompt = format!("{} ", "λ >".bright_green().bold());

    loop {
        match rl.readline(&prompt) {
            Ok(line) => {
                let line: String = line;
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }

                // Add to history
                let _ = rl.add_history_entry(&line);

                // Check for quit command
                if input == "(quit)" || input == "(exit)" {
                    println!("Goodbye!");
                    break;
                }

                // Check for help command
                if input == ":?" || input == ":help" {
                    print_help();
                    continue;
                }

                if input == ":q" || input == ":quit" {
                    println!("Goodbye!");
                    break;
                }

                // Evaluate the expression
                match engine.run(line.clone()) {
                    Ok(results) => {
                        for result in results {
                            if !matches!(result, SteelVal::Void) {
                                print!("{} ", "=>".bright_blue().bold());
                                println!("{}", result);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: {}", "Error".bright_red().bold(), e);
                    }
                }

                let _ = std::io::stdout().flush();
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("Goodbye!");
                break;
            }
            Err(err) => {
                eprintln!("{}: {:?}", "Error".bright_red().bold(), err);
                break;
            }
        }
    }

    // Save history
    if let Err(e) = rl.save_history(&history_path) {
        eprintln!(
            "{}: Failed to save history: {}",
            "Warning".bright_yellow(),
            e
        );
    }

    Ok(())
}

/// Print help message
fn print_help() {
    println!(
        "
    {}           -- toggles the timing of expressions
    {} -- displays help dialog
    {}    -- exits the REPL

    {}   -- show rflasher SPI commands
    ",
        ":time".bright_cyan(),
        ":? | :help".bright_cyan(),
        ":q | :quit".bright_cyan(),
        "(rflasher-help)".bright_cyan(),
    );
}

/// Run the Steel REPL with the given SPI master
///
/// This takes ownership of the SPI master and provides it to the Scheme
/// environment for executing SPI commands.
pub fn run_repl<M: SpiMaster + Send + 'static>(master: M) -> Result<(), ReplError> {
    run_repl_boxed(Box::new(master))
}

/// Run a Steel script with a boxed SPI master
pub fn run_script_boxed(master: BoxedSpiMaster, script: String) -> Result<(), ReplError> {
    let mut engine = Engine::new();

    let master = Arc::new(Mutex::new(master));

    let module = spi_module::create_spi_module_boxed(Arc::clone(&master));
    engine.register_module(module);

    let constants_module = spi_module::create_constants_module();
    engine.register_module(constants_module);

    engine
        .run(
            r#"
        (require-builtin rflasher/spi)
        (require-builtin rflasher/spi25)
    "#,
        )
        .map_err(|e| ReplError::SteelError(format!("{}", e)))?;

    match engine.run(script) {
        Ok(results) => {
            for result in results {
                if !matches!(result, SteelVal::Void) {
                    print!("{} ", "=>".bright_blue().bold());
                    println!("{}", result);
                }
            }
        }
        Err(e) => {
            return Err(ReplError::SteelError(format!("{}", e)));
        }
    }

    Ok(())
}

/// Run a Steel script file with the given SPI master
pub fn run_script<M: SpiMaster + Send + 'static>(
    master: M,
    script: String,
) -> Result<(), ReplError> {
    run_script_boxed(Box::new(master), script)
}
