//! Narrow EM100 operations for the hardware smoke harness. Never changes voltage.
use em100::{DeviceSession, Em100, fpga, parse_dcfg};
use std::{env, fs, io};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const CAPACITY: usize = 64 * 1024 * 1024;

fn error(message: &str) -> io::Error {
    io::Error::other(message)
}

async fn run(args: &[String]) -> Result<()> {
    let command = args.first().ok_or_else(|| error("missing command"))?;
    let serial = args.get(1).ok_or_else(|| error("missing EM100 serial"))?;
    // Validate configuration before opening hardware: profile loading can otherwise
    // switch the FPGA voltage automatically, which is unsafe for the Pico wiring.
    let configuration = if command == "configure" {
        let path = args
            .get(2)
            .ok_or_else(|| error("missing chip configuration"))?;
        let chip = parse_dcfg(&fs::read(path)?)?;
        let supply =
            chip.init.iter().take(chip.init_len).find_map(|e| {
                (e[0] == 0x11 && e[1] == 0x04).then(|| u16::from_be_bytes([e[2], e[3]]))
            });
        if supply != Some(3300) || chip.size as usize > CAPACITY {
            return Err(error("requires a 3.3 V profile within the 64 MiB rem100 limit").into());
        }
        let data = fs::read(args.get(3).ok_or_else(|| error("missing image"))?)?;
        if data.len() != chip.size as usize {
            return Err(error("image length differs from chip capacity").into());
        }
        Some((chip.clone(), data))
    } else {
        None
    };
    let serial_number = serial.strip_prefix("EM").unwrap_or(serial).parse()?;
    let device = Em100::open(None, None, Some(serial_number)).await?;
    let mut session = DeviceSession::new(device).await;
    session.stop_for_mutation().await?;
    match command.as_str() {
        "configure" => {
            let (chip, data) = configuration.unwrap();
            // Legacy profiles omit SFDP. C9 resets the table-upload pointer;
            // writing zero to it does NOT invalidate the previous table. Replace
            // all 256 bytes with FF before the profile optionally supplies SFDP.
            fpga::write_fpga_register(session.device_mut(), 0xc9, 1).await?;
            for _ in 0..128 {
                fpga::write_fpga_register(session.device_mut(), 0xc1, 0xffff).await?;
            }
            session.configure_chip(&chip).await?;
            // Start in ordinary 3-byte mode, allowing B7 entry for large parts.
            // Dedicated 4-byte opcodes remain independent of this address mode.
            session
                .set_address_mode(3, chip.size > 16 * 1024 * 1024)
                .await?;
            session.write_memory(&data, 0, None).await?;
            let actual = session.read_memory(0, data.len(), None).await?;
            if data != actual {
                return Err(error("EM100 preload verification failed").into());
            }
            session.set_emulation_state(true).await?;
            println!(
                "Configured {} {} bytes; SDRAM verified; 3-byte startup",
                chip.name,
                data.len()
            );
        }
        "backup" | "capture" => {
            let length: usize = args
                .get(2)
                .ok_or_else(|| error("missing length"))?
                .parse()?;
            if length == 0 || length > CAPACITY {
                return Err(error("invalid capture length").into());
            }
            let path = args.get(3).ok_or_else(|| error("missing output path"))?;
            // Refuse to overwrite a recovery image or an earlier capture.
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            let data = session.read_memory(0, length, None).await?;
            use std::io::Write;
            file.write_all(&data)?;
            file.sync_all()?;
            session.set_emulation_state(command == "capture").await?;
            println!("Captured {} bytes", data.len());
        }
        "restore" => {
            let data = fs::read(args.get(2).ok_or_else(|| error("missing recovery image"))?)?;
            if data.len() != CAPACITY {
                return Err(error("recovery image must cover 64 MiB").into());
            }
            session.write_memory(&data, 0, None).await?;
            let actual = session.read_memory(0, data.len(), None).await?;
            session.set_emulation_state(false).await?;
            if actual != data {
                return Err(error("restoration verification failed").into());
            }
            println!("Restored and verified 64 MiB; emulation stopped");
        }
        "stop" => println!("Emulation stopped"),
        _ => return Err(error("expected configure, backup, capture, restore, or stop").into()),
    }
    Ok(())
}

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if let Err(error) = futures_lite::future::block_on(run(&args)) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
