//! Terminal prompt for choosing between several connected USB programmers.

use rflasher_programmers::UsbDeviceSummary;
use std::fmt::Display;
use std::io::{BufRead, IsTerminal, Write};

/// Whether the user can be asked on the terminal.
pub fn can_prompt(non_interactive: bool) -> bool {
    !non_interactive && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Ask on the terminal which of `devices` to use.
pub fn prompt_usb_device(devices: &[UsbDeviceSummary]) -> Result<usize, String> {
    choose_device(
        devices,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr().lock(),
    )
    .map_err(|e| e.to_string())
}

fn choose_device(
    devices: &[impl Display],
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<usize, Box<dyn std::error::Error>> {
    writeln!(output, "Multiple USB programmers found:")?;
    for (index, device) in devices.iter().enumerate() {
        writeln!(output, "{index}: {device}")?;
    }
    write!(output, "Selection: ")?;
    output.flush()?;

    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err("Device selection cancelled (end of input)".into());
    }
    let line = line.trim();
    line.parse()
        .map_err(|_| format!("Invalid device selection: {line:?}").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn lists_devices_and_reads_the_selection() {
        let mut output = Vec::new();
        let index = choose_device(&["first", "second"], &mut Cursor::new(" 1\n"), &mut output);
        assert_eq!(index.unwrap(), 1);
        let output = String::from_utf8(output).unwrap();
        assert!(output.ends_with("0: first\n1: second\nSelection: "));
    }

    #[test]
    fn invalid_selection_and_eof_never_default_to_a_device() {
        for input in ["", "\n", "bad\n", "-1\n"] {
            let index = choose_device(
                &["first", "second"],
                &mut Cursor::new(input),
                &mut Vec::new(),
            );
            assert!(index.is_err(), "{input:?}");
        }
    }
}
