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
use rustyline::Editor;
use rustyline::config::Configurer;
use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;
use steel_parser::interner::InternedString;

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

    for name in spi_module::KNOWN_SYMBOLS {
        globals
            .insert(InternedString::try_get(name).unwrap_or_else(|| InternedString::from(*name)));
    }

    globals
}

/// Run the Steel REPL with the given SPI master
///
/// The REPL's Scheme callbacks are synchronous; each SPI operation is
/// bridged to the async `SpiMaster` API with a local `block_on`.
pub fn run_repl<M: SpiMaster + Send + 'static>(master: M) -> Result<(), ReplError> {
    let mut engine = Engine::new();

    // Wrap master in Arc<Mutex> for thread-safe access from Steel
    let master = Arc::new(Mutex::new(master));

    // Register the SPI module
    let module = spi_module::create_spi_module(Arc::clone(&master));
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
    {} -- displays help dialog
    {}    -- exits the REPL

    {}   -- show rflasher SPI commands
    ",
        ":? | :help".bright_cyan(),
        ":q | :quit".bright_cyan(),
        "(rflasher-help)".bright_cyan(),
    );
}

/// Run the Steel REPL with the given SPI master
///
/// This takes ownership of the SPI master and provides it to the Scheme
/// environment for executing SPI commands.
pub fn run_script<M: SpiMaster + Send + 'static>(
    master: M,
    script: String,
) -> Result<(), ReplError> {
    let mut engine = Engine::new();

    let master = Arc::new(Mutex::new(master));

    let module = spi_module::create_spi_module(Arc::clone(&master));
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

#[cfg(test)]
mod tests {
    use super::*;
    use rflasher_core::error::Result as CoreResult;
    use rflasher_core::programmer::SpiFeatures;
    use rflasher_core::spi::{SpiCommand, opcodes};

    struct MockMaster {
        status_polls_remaining: usize,
        polls: usize,
        delays_us: Vec<u32>,
    }

    impl MockMaster {
        fn new(status_polls_remaining: usize) -> Self {
            Self {
                status_polls_remaining,
                polls: 0,
                delays_us: Vec::new(),
            }
        }
    }

    impl SpiMaster for MockMaster {
        fn features(&self) -> SpiFeatures {
            SpiFeatures::FOUR_BYTE_ADDR
        }

        fn max_read_len(&self) -> usize {
            256
        }

        fn max_write_len(&self) -> usize {
            256
        }

        async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
            if cmd.opcode == opcodes::RDSR && !cmd.read_buf.is_empty() {
                self.polls += 1;
                if self.status_polls_remaining > 0 {
                    self.status_polls_remaining -= 1;
                    cmd.read_buf[0] = opcodes::SR1_WIP;
                } else {
                    cmd.read_buf[0] = 0;
                }
            }
            Ok(())
        }

        async fn delay_us(&mut self, us: u32) {
            self.delays_us.push(us);
        }
    }

    fn make_engine(master: MockMaster) -> (Engine, Arc<Mutex<MockMaster>>) {
        let mut engine = Engine::new();
        let shared = Arc::new(Mutex::new(master));
        engine.register_module(spi_module::create_spi_module(Arc::clone(&shared)));
        engine.register_module(spi_module::create_constants_module());
        engine
            .run(
                r#"
                (require-builtin rflasher/spi)
                (require-builtin rflasher/spi25)
            "#,
            )
            .unwrap();
        (engine, shared)
    }

    #[test]
    fn all_collected_globals_resolve_in_engine() {
        let (mut engine, _) = make_engine(MockMaster::new(0));
        let globals = collect_globals(&engine);
        assert_eq!(globals.len(), spi_module::KNOWN_SYMBOLS.len());

        for name in spi_module::KNOWN_SYMBOLS {
            engine
                .run(name.to_string())
                .unwrap_or_else(|e| panic!("known REPL global `{name}` did not resolve: {e}"));
        }
    }

    #[test]
    fn repl_rejects_negative_lengths_and_non_ascii_hex_without_panicking() {
        let (mut engine, _) = make_engine(MockMaster::new(0));

        for bad_expr in [
            "(make-bytes -1 0)",
            "(make-bytes 4 -1)",
            "(make-bytes 4 256)",
            "(random-bytes -5)",
            "(spi-read READ 0 -1)",
            "(spi-read READ -1 4)",
            "(spi-read READ #x1000000 4)",
            "(spi-execute READ -1 0 #f 4)",
            "(wait-ready -1)",
            "(list->bytes '(-1))",
            "(list->bytes '(256))",
            r#"(hex->bytes "a€")"#,
            r#"(hex->bytes "é")"#,
            r#"(hex->bytes "abc")"#,
        ] {
            assert!(
                engine.run(bad_expr.to_string()).is_err(),
                "expression `{bad_expr}` should fail cleanly"
            );
        }

        // Valid hex formats including 0x/0X and whitespace work.
        let vals = engine
            .run(r#"(hex->bytes "0xde, 0XAD be ef")"#.to_string())
            .unwrap();
        assert_eq!(vals.len(), 1);
    }

    #[test]
    fn wait_ready_polls_at_least_once_for_short_timeout() {
        let (mut engine, shared) = make_engine(MockMaster::new(0));
        let res = engine.run("(wait-ready 50)".to_string()).unwrap();
        assert_eq!(res, vec![SteelVal::BoolV(true)]);
        assert_eq!(shared.lock().unwrap().polls, 1);
    }

    #[test]
    fn wait_ready_uses_100us_poll_interval_and_caller_timeout() {
        let (mut engine, shared) = make_engine(MockMaster::new(5));
        let res = engine.run("(wait-ready 10000)".to_string()).unwrap();
        assert_eq!(res, vec![SteelVal::BoolV(true)]);
        let master = shared.lock().unwrap();
        assert_eq!(master.polls, 6);
        assert_eq!(master.delays_us, vec![100; 5]);
    }
}
