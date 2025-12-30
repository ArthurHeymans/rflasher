//! REPL command implementation

use rflasher_flash::open_spi_programmer;
use std::path::Path;

/// Run the Scheme REPL or execute a script
pub fn cmd_repl(programmer: &str, script: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    // Open the programmer
    let master = open_spi_programmer(programmer)?;

    if let Some(script_path) = script {
        // Run a script file
        let script_content = std::fs::read_to_string(script_path)?;
        rflasher_repl::run_script_boxed(master, script_content)?;
    } else {
        // Interactive REPL
        rflasher_repl::run_repl_boxed(master)?;
    }

    Ok(())
}
