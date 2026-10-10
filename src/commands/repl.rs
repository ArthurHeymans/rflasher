//! REPL command implementation

use rflasher_programmers::{ChooseUsbDevice, open_spi_programmer};
use std::path::Path;

/// Run the Scheme REPL or execute a script
pub async fn cmd_repl(
    programmer: &str,
    script: Option<&Path>,
    choose_usb_device: Option<ChooseUsbDevice<'_>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Open the programmer
    let master = open_spi_programmer(programmer, choose_usb_device).await?;

    if let Some(script_path) = script {
        // Run a script file
        let script_content = std::fs::read_to_string(script_path)?;
        rflasher_repl::run_script(master, script_content)?;
    } else {
        // Interactive REPL
        rflasher_repl::run_repl(master)?;
    }

    Ok(())
}
