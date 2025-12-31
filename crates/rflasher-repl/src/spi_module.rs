//! SPI module for Steel Scheme
//!
//! This module exposes raw SPI commands and helpers to the Scheme environment.

use rflasher_core::programmer::SpiMaster;
use rflasher_core::spi::opcodes;
use rflasher_core::spi::{AddressWidth, IoMode, SpiCommand};
use std::sync::{Arc, Mutex};
use steel::rvals::SteelVal;
use steel::steel_vm::builtin::BuiltInModule;
use steel::steel_vm::register_fn::RegisterFn;

/// Type alias for boxed SPI master
pub type BoxedSpiMaster = Box<dyn SpiMaster + Send>;

/// Type alias for the shared SPI master
type SharedMaster<M> = Arc<Mutex<M>>;

/// Create the SPI module with functions bound to a boxed SPI master
pub fn create_spi_module_boxed(master: Arc<Mutex<BoxedSpiMaster>>) -> BuiltInModule {
    create_spi_module(master)
}

/// Create the SPI module with functions bound to the given master
pub fn create_spi_module<M: SpiMaster + Send + 'static>(master: SharedMaster<M>) -> BuiltInModule {
    let mut module = BuiltInModule::new("rflasher/spi");

    // Register SPI commands that need the master
    register_spi_commands(&mut module, &master);

    // Register byte utilities (don't need master)
    register_byte_utilities(&mut module);

    // Help function - named rflasher-help to avoid conflict with Steel's built-in help
    module.register_fn("rflasher-help", || {
        print_help();
        SteelVal::Void
    });

    module
}

/// Register all SPI command functions that require the master
fn register_spi_commands<M: SpiMaster + Send + 'static>(
    module: &mut BuiltInModule,
    master: &SharedMaster<M>,
) {
    // Low-level SPI commands
    let m = Arc::clone(master);
    module.register_fn(
        "spi-execute",
        move |opcode: isize,
              addr: SteelVal,
              dummy: isize,
              write_data: SteelVal,
              read_len: isize| {
            spi_execute(
                &m,
                opcode as u8,
                addr,
                dummy as u8,
                write_data,
                read_len as usize,
            )
        },
    );

    let m = Arc::clone(master);
    module.register_fn("spi-simple", move |opcode: isize| {
        spi_simple(&m, opcode as u8)
    });

    let m = Arc::clone(master);
    module.register_fn("spi-read-reg", move |opcode: isize, len: isize| {
        spi_read_reg(&m, opcode as u8, len as usize)
    });

    let m = Arc::clone(master);
    module.register_fn("spi-write-reg", move |opcode: isize, data: SteelVal| {
        spi_write_reg(&m, opcode as u8, data)
    });

    let m = Arc::clone(master);
    module.register_fn("spi-read", move |opcode: isize, addr: isize, len: isize| {
        spi_read_3b(&m, opcode as u8, addr as u32, len as usize)
    });

    let m = Arc::clone(master);
    module.register_fn(
        "spi-read-4b",
        move |opcode: isize, addr: isize, len: isize| {
            spi_read_4b(&m, opcode as u8, addr as u32, len as usize)
        },
    );

    let m = Arc::clone(master);
    module.register_fn(
        "spi-write",
        move |opcode: isize, addr: isize, data: SteelVal| {
            spi_write_3b(&m, opcode as u8, addr as u32, data)
        },
    );

    let m = Arc::clone(master);
    module.register_fn(
        "spi-write-4b",
        move |opcode: isize, addr: isize, data: SteelVal| {
            spi_write_4b(&m, opcode as u8, addr as u32, data)
        },
    );

    // Dual I/O reads
    let m = Arc::clone(master);
    module.register_fn(
        "spi-read-dual-out",
        move |opcode: isize, addr: isize, len: isize| {
            spi_read_multi(
                &m,
                opcode as u8,
                addr as u32,
                len as usize,
                IoMode::DualOut,
                AddressWidth::ThreeByte,
            )
        },
    );

    let m = Arc::clone(master);
    module.register_fn(
        "spi-read-dual-io",
        move |opcode: isize, addr: isize, len: isize| {
            spi_read_multi(
                &m,
                opcode as u8,
                addr as u32,
                len as usize,
                IoMode::DualIo,
                AddressWidth::ThreeByte,
            )
        },
    );

    // Quad I/O reads
    let m = Arc::clone(master);
    module.register_fn(
        "spi-read-quad-out",
        move |opcode: isize, addr: isize, len: isize| {
            spi_read_multi(
                &m,
                opcode as u8,
                addr as u32,
                len as usize,
                IoMode::QuadOut,
                AddressWidth::ThreeByte,
            )
        },
    );

    let m = Arc::clone(master);
    module.register_fn(
        "spi-read-quad-io",
        move |opcode: isize, addr: isize, len: isize| {
            spi_read_multi(
                &m,
                opcode as u8,
                addr as u32,
                len as usize,
                IoMode::QuadIo,
                AddressWidth::ThreeByte,
            )
        },
    );

    // High-level helper functions
    let m = Arc::clone(master);
    module.register_fn("read-jedec-id", move || read_jedec_id(&m));

    let m = Arc::clone(master);
    module.register_fn("read-status1", move || read_status(&m, opcodes::RDSR));

    let m = Arc::clone(master);
    module.register_fn("read-status2", move || read_status(&m, opcodes::RDSR2));

    let m = Arc::clone(master);
    module.register_fn("read-status3", move || read_status(&m, opcodes::RDSR3));

    let m = Arc::clone(master);
    module.register_fn("write-enable", move || write_simple(&m, opcodes::WREN));

    let m = Arc::clone(master);
    module.register_fn("write-disable", move || write_simple(&m, opcodes::WRDI));

    let m = Arc::clone(master);
    module.register_fn("chip-erase", move || chip_erase(&m));

    let m = Arc::clone(master);
    module.register_fn("reset-enable", move || write_simple(&m, opcodes::RSTEN));

    let m = Arc::clone(master);
    module.register_fn("reset", move || write_simple(&m, opcodes::RST));

    let m = Arc::clone(master);
    module.register_fn("enter-4byte-mode", move || write_simple(&m, opcodes::EN4B));

    let m = Arc::clone(master);
    module.register_fn("exit-4byte-mode", move || write_simple(&m, opcodes::EX4B));

    let m = Arc::clone(master);
    module.register_fn("deep-power-down", move || write_simple(&m, opcodes::DP));

    let m = Arc::clone(master);
    module.register_fn("release-power-down", move || write_simple(&m, opcodes::RDP));

    let m = Arc::clone(master);
    module.register_fn("read-sfdp", move |addr: isize, len: isize| {
        read_sfdp(&m, addr as u32, len as usize)
    });

    let m = Arc::clone(master);
    module.register_fn("is-busy?", move || is_busy(&m));

    let m = Arc::clone(master);
    module.register_fn("wait-ready", move |timeout_us: isize| {
        wait_ready(&m, timeout_us as u32)
    });

    let m = Arc::clone(master);
    module.register_fn("write-status1", move |value: isize| {
        write_status(&m, opcodes::WRSR, value as u8)
    });

    let m = Arc::clone(master);
    module.register_fn("write-status2", move |value: isize| {
        write_status(&m, opcodes::WRSR2, value as u8)
    });

    let m = Arc::clone(master);
    module.register_fn("write-status3", move |value: isize| {
        write_status(&m, opcodes::WRSR3, value as u8)
    });

    let m = Arc::clone(master);
    module.register_fn("sector-erase", move |addr: isize| {
        erase_block(&m, opcodes::SE_20, addr as u32, false)
    });

    let m = Arc::clone(master);
    module.register_fn("block-erase-32k", move |addr: isize| {
        erase_block(&m, opcodes::BE_52, addr as u32, false)
    });

    let m = Arc::clone(master);
    module.register_fn("block-erase-64k", move |addr: isize| {
        erase_block(&m, opcodes::BE_D8, addr as u32, false)
    });

    // Page program helpers
    let m = Arc::clone(master);
    module.register_fn("page-program", move |addr: isize, data: SteelVal| {
        page_program(&m, addr as u32, data, false)
    });

    let m = Arc::clone(master);
    module.register_fn("page-program-4b", move |addr: isize, data: SteelVal| {
        page_program(&m, addr as u32, data, true)
    });
}

/// Register byte vector utility functions (independent of master)
fn register_byte_utilities(module: &mut BuiltInModule) {
    module.register_fn("make-bytes", |len: isize, fill: isize| -> SteelVal {
        let bytes: Vec<u8> = vec![fill as u8; len as usize];
        bytes_to_steel(&bytes)
    });

    module.register_fn("random-bytes", |len: isize| -> SteelVal {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut bytes = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let s = RandomState::new();
            let mut hasher = s.build_hasher();
            hasher.write_u8(0);
            bytes.push(hasher.finish() as u8);
        }
        bytes_to_steel(&bytes)
    });

    module.register_fn("bytes-length", |data: SteelVal| -> Result<isize, String> {
        let bytes = steel_to_bytes(&data)?;
        Ok(bytes.len() as isize)
    });

    module.register_fn(
        "bytes-ref",
        |data: SteelVal, index: isize| -> Result<isize, String> {
            let bytes = steel_to_bytes(&data)?;
            bytes
                .get(index as usize)
                .map(|&b| b as isize)
                .ok_or_else(|| format!("index {} out of bounds", index))
        },
    );

    module.register_fn(
        "bytes->list",
        |data: SteelVal| -> Result<SteelVal, String> {
            let bytes = steel_to_bytes(&data)?;
            Ok(SteelVal::ListV(
                bytes.iter().map(|&b| SteelVal::IntV(b as isize)).collect(),
            ))
        },
    );

    module.register_fn(
        "list->bytes",
        |list: SteelVal| -> Result<SteelVal, String> {
            match list {
                SteelVal::ListV(items) => {
                    let bytes: Result<Vec<u8>, String> = items
                        .iter()
                        .map(|v| match v {
                            SteelVal::IntV(i) => Ok(*i as u8),
                            _ => Err("list->bytes: expected list of integers".to_string()),
                        })
                        .collect();
                    Ok(bytes_to_steel(&bytes?))
                }
                _ => Err("list->bytes: expected list".to_string()),
            }
        },
    );

    module.register_fn("bytes->hex", |data: SteelVal| -> Result<String, String> {
        let bytes = steel_to_bytes(&data)?;
        Ok(bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" "))
    });

    module.register_fn("hex->bytes", |hex: String| -> Result<SteelVal, String> {
        let hex = hex.replace(" ", "").replace("0x", "").replace(",", "");
        if !hex.len().is_multiple_of(2) {
            return Err("hex string must have even length".to_string());
        }
        let bytes: Result<Vec<u8>, _> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
            .collect();
        bytes
            .map(|b| bytes_to_steel(&b))
            .map_err(|e| format!("invalid hex: {}", e))
    });
}

/// Create the SPI25 constants module
pub fn create_constants_module() -> BuiltInModule {
    let mut module = BuiltInModule::new("rflasher/spi25");

    // Write control
    module.register_value("WREN", SteelVal::IntV(opcodes::WREN as isize));
    module.register_value("WRDI", SteelVal::IntV(opcodes::WRDI as isize));
    module.register_value("EWSR", SteelVal::IntV(opcodes::EWSR as isize));

    // Status registers
    module.register_value("RDSR", SteelVal::IntV(opcodes::RDSR as isize));
    module.register_value("RDSR2", SteelVal::IntV(opcodes::RDSR2 as isize));
    module.register_value("RDSR3", SteelVal::IntV(opcodes::RDSR3 as isize));
    module.register_value("WRSR", SteelVal::IntV(opcodes::WRSR as isize));
    module.register_value("WRSR2", SteelVal::IntV(opcodes::WRSR2 as isize));
    module.register_value("WRSR3", SteelVal::IntV(opcodes::WRSR3 as isize));

    // Identification
    module.register_value("RDID", SteelVal::IntV(opcodes::RDID as isize));
    module.register_value("REMS", SteelVal::IntV(opcodes::REMS as isize));
    module.register_value("RES", SteelVal::IntV(opcodes::RES as isize));
    module.register_value("RDUID", SteelVal::IntV(opcodes::RDUID as isize));

    // Read commands - 3-byte address
    module.register_value("READ", SteelVal::IntV(opcodes::READ as isize));
    module.register_value("FAST_READ", SteelVal::IntV(opcodes::FAST_READ as isize));

    // Read commands - 4-byte address
    module.register_value("READ_4B", SteelVal::IntV(opcodes::READ_4B as isize));
    module.register_value(
        "FAST_READ_4B",
        SteelVal::IntV(opcodes::FAST_READ_4B as isize),
    );

    // Dual/Quad read - 3-byte address
    module.register_value("DOR", SteelVal::IntV(opcodes::DOR as isize));
    module.register_value("DIOR", SteelVal::IntV(opcodes::DIOR as isize));
    module.register_value("QOR", SteelVal::IntV(opcodes::QOR as isize));
    module.register_value("QIOR", SteelVal::IntV(opcodes::QIOR as isize));

    // Dual/Quad read - 4-byte address
    module.register_value("DOR_4B", SteelVal::IntV(opcodes::DOR_4B as isize));
    module.register_value("DIOR_4B", SteelVal::IntV(opcodes::DIOR_4B as isize));
    module.register_value("QOR_4B", SteelVal::IntV(opcodes::QOR_4B as isize));
    module.register_value("QIOR_4B", SteelVal::IntV(opcodes::QIOR_4B as isize));

    // Page Program
    module.register_value("PP", SteelVal::IntV(opcodes::PP as isize));
    module.register_value("PP_4B", SteelVal::IntV(opcodes::PP_4B as isize));
    module.register_value("QPP", SteelVal::IntV(opcodes::QPP as isize));
    module.register_value("QPP_4B", SteelVal::IntV(opcodes::QPP_4B as isize));

    // Erase commands - 3-byte address
    module.register_value("SE_20", SteelVal::IntV(opcodes::SE_20 as isize));
    module.register_value("BE_52", SteelVal::IntV(opcodes::BE_52 as isize));
    module.register_value("BE_D8", SteelVal::IntV(opcodes::BE_D8 as isize));
    module.register_value("CE_60", SteelVal::IntV(opcodes::CE_60 as isize));
    module.register_value("CE_C7", SteelVal::IntV(opcodes::CE_C7 as isize));

    // Erase commands - 4-byte address
    module.register_value("SE_21", SteelVal::IntV(opcodes::SE_21 as isize));
    module.register_value("BE_5C", SteelVal::IntV(opcodes::BE_5C as isize));
    module.register_value("BE_DC", SteelVal::IntV(opcodes::BE_DC as isize));

    // 4-byte address mode
    module.register_value("EN4B", SteelVal::IntV(opcodes::EN4B as isize));
    module.register_value("EX4B", SteelVal::IntV(opcodes::EX4B as isize));
    module.register_value("RDEAR", SteelVal::IntV(opcodes::RDEAR as isize));
    module.register_value("WREAR", SteelVal::IntV(opcodes::WREAR as isize));

    // Power management
    module.register_value("DP", SteelVal::IntV(opcodes::DP as isize));
    module.register_value("RDP", SteelVal::IntV(opcodes::RDP as isize));

    // Security registers
    module.register_value("ERSR", SteelVal::IntV(opcodes::ERSR as isize));
    module.register_value("PRSR", SteelVal::IntV(opcodes::PRSR as isize));
    module.register_value("RDSR_SEC", SteelVal::IntV(opcodes::RDSR_SEC as isize));

    // QPI mode
    module.register_value("EQIO", SteelVal::IntV(opcodes::EQIO as isize));
    module.register_value("RSTQIO", SteelVal::IntV(opcodes::RSTQIO as isize));

    // Software reset
    module.register_value("RSTEN", SteelVal::IntV(opcodes::RSTEN as isize));
    module.register_value("RST", SteelVal::IntV(opcodes::RST as isize));

    // SFDP
    module.register_value("RDSFDP", SteelVal::IntV(opcodes::RDSFDP as isize));

    // Suspend/Resume
    module.register_value("SUSPEND", SteelVal::IntV(opcodes::SUSPEND as isize));
    module.register_value("RESUME", SteelVal::IntV(opcodes::RESUME as isize));

    // Status register 1 bits
    module.register_value("SR1_WIP", SteelVal::IntV(opcodes::SR1_WIP as isize));
    module.register_value("SR1_WEL", SteelVal::IntV(opcodes::SR1_WEL as isize));
    module.register_value("SR1_BP0", SteelVal::IntV(opcodes::SR1_BP0 as isize));
    module.register_value("SR1_BP1", SteelVal::IntV(opcodes::SR1_BP1 as isize));
    module.register_value("SR1_BP2", SteelVal::IntV(opcodes::SR1_BP2 as isize));
    module.register_value("SR1_TB", SteelVal::IntV(opcodes::SR1_TB as isize));
    module.register_value("SR1_SEC", SteelVal::IntV(opcodes::SR1_SEC as isize));
    module.register_value("SR1_SRP0", SteelVal::IntV(opcodes::SR1_SRP0 as isize));

    // Status register 2 bits
    module.register_value("SR2_SRP1", SteelVal::IntV(opcodes::SR2_SRP1 as isize));
    module.register_value("SR2_QE", SteelVal::IntV(opcodes::SR2_QE as isize));
    module.register_value("SR2_BP3", SteelVal::IntV(opcodes::SR2_BP3 as isize));
    module.register_value("SR2_LB1", SteelVal::IntV(opcodes::SR2_LB1 as isize));
    module.register_value("SR2_LB2", SteelVal::IntV(opcodes::SR2_LB2 as isize));
    module.register_value("SR2_LB3", SteelVal::IntV(opcodes::SR2_LB3 as isize));
    module.register_value("SR2_CMP", SteelVal::IntV(opcodes::SR2_CMP as isize));
    module.register_value("SR2_SUS", SteelVal::IntV(opcodes::SR2_SUS as isize));

    // Status register 3 bits
    module.register_value("SR3_WPS", SteelVal::IntV(opcodes::SR3_WPS as isize));
    module.register_value("SR3_ADP", SteelVal::IntV(opcodes::SR3_ADP as isize));
    module.register_value("SR3_ADS", SteelVal::IntV(opcodes::SR3_ADS as isize));

    module
}

// =============================================================================
// Helper functions
// =============================================================================

/// Convert bytes to Steel value (as a list of integers)
fn bytes_to_steel(bytes: &[u8]) -> SteelVal {
    SteelVal::ListV(bytes.iter().map(|&b| SteelVal::IntV(b as isize)).collect())
}

/// Convert Steel value to bytes
fn steel_to_bytes(val: &SteelVal) -> Result<Vec<u8>, String> {
    match val {
        SteelVal::ListV(items) => items
            .iter()
            .map(|v| match v {
                SteelVal::IntV(i) => Ok(*i as u8),
                _ => Err("expected list of integers".to_string()),
            })
            .collect(),
        SteelVal::StringV(s) => Ok(s.as_bytes().to_vec()),
        _ => Err("expected list or string".to_string()),
    }
}

// =============================================================================
// SPI command implementations
// =============================================================================

fn spi_execute<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    addr: SteelVal,
    dummy: u8,
    write_data: SteelVal,
    read_len: usize,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let address = match addr {
        SteelVal::BoolV(false) => None,
        SteelVal::IntV(a) => Some(a as u32),
        _ => return Err("address must be #f or integer".to_string()),
    };

    let write_bytes = match &write_data {
        SteelVal::BoolV(false) => vec![],
        _ => steel_to_bytes(&write_data)?,
    };

    let mut read_buf = vec![0u8; read_len];

    let address_width = match address {
        None => AddressWidth::None,
        Some(a) if a <= 0xFFFFFF => AddressWidth::ThreeByte,
        Some(_) => AddressWidth::FourByte,
    };

    let mut cmd = SpiCommand {
        opcode,
        address,
        address_width,
        io_mode: IoMode::Single,
        dummy_cycles: dummy,
        write_data: &write_bytes,
        read_buf: &mut read_buf,
    };

    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    if read_len > 0 {
        Ok(bytes_to_steel(&read_buf))
    } else {
        Ok(SteelVal::BoolV(true))
    }
}

fn spi_simple<M: SpiMaster>(master: &SharedMaster<M>, opcode: u8) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut cmd = SpiCommand::simple(opcode);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(SteelVal::BoolV(true))
}

fn spi_read_reg<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    len: usize,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut buf = vec![0u8; len];
    let mut cmd = SpiCommand::read_reg(opcode, &mut buf);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    if len == 1 {
        Ok(SteelVal::IntV(buf[0] as isize))
    } else {
        Ok(bytes_to_steel(&buf))
    }
}

fn spi_write_reg<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    data: SteelVal,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let bytes = steel_to_bytes(&data)?;
    let mut cmd = SpiCommand::write_reg(opcode, &bytes);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(SteelVal::BoolV(true))
}

fn spi_read_3b<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    addr: u32,
    len: usize,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut buf = vec![0u8; len];
    let mut cmd = SpiCommand::read_3b(opcode, addr, &mut buf);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(bytes_to_steel(&buf))
}

fn spi_read_4b<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    addr: u32,
    len: usize,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut buf = vec![0u8; len];
    let mut cmd = SpiCommand::read_4b(opcode, addr, &mut buf);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(bytes_to_steel(&buf))
}

fn spi_read_multi<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    addr: u32,
    len: usize,
    io_mode: IoMode,
    addr_width: AddressWidth,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut buf = vec![0u8; len];

    let dummy = match io_mode {
        IoMode::DualOut | IoMode::QuadOut => 8,
        IoMode::DualIo => 4,
        IoMode::QuadIo => 6,
        _ => 0,
    };

    let mut cmd = SpiCommand {
        opcode,
        address: Some(addr),
        address_width: addr_width,
        io_mode,
        dummy_cycles: dummy,
        write_data: &[],
        read_buf: &mut buf,
    };

    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(bytes_to_steel(&buf))
}

fn spi_write_3b<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    addr: u32,
    data: SteelVal,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let bytes = steel_to_bytes(&data)?;
    let mut cmd = SpiCommand::write_3b(opcode, addr, &bytes);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(SteelVal::BoolV(true))
}

fn spi_write_4b<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    addr: u32,
    data: SteelVal,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let bytes = steel_to_bytes(&data)?;
    let mut cmd = SpiCommand::write_4b(opcode, addr, &bytes);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(SteelVal::BoolV(true))
}

fn read_jedec_id<M: SpiMaster>(master: &SharedMaster<M>) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut buf = [0u8; 3];
    let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut buf);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    let manufacturer = buf[0] as isize;
    let device = ((buf[1] as isize) << 8) | (buf[2] as isize);

    Ok(SteelVal::ListV(
        vec![SteelVal::IntV(manufacturer), SteelVal::IntV(device)].into(),
    ))
}

fn read_status<M: SpiMaster>(master: &SharedMaster<M>, opcode: u8) -> Result<isize, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut buf = [0u8; 1];
    let mut cmd = SpiCommand::read_reg(opcode, &mut buf);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(buf[0] as isize)
}

fn write_simple<M: SpiMaster>(master: &SharedMaster<M>, opcode: u8) -> Result<bool, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut cmd = SpiCommand::simple(opcode);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(true)
}

fn write_status<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    value: u8,
) -> Result<bool, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    // First send WREN
    let mut wren = SpiCommand::simple(opcodes::WREN);
    m.execute(&mut wren)
        .map_err(|e| format!("WREN error: {}", e))?;

    // Then write status
    let data = [value];
    let mut cmd = SpiCommand::write_reg(opcode, &data);
    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(true)
}

fn read_sfdp<M: SpiMaster>(
    master: &SharedMaster<M>,
    addr: u32,
    len: usize,
) -> Result<SteelVal, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    let mut buf = vec![0u8; len];

    let mut cmd = SpiCommand {
        opcode: opcodes::RDSFDP,
        address: Some(addr),
        address_width: AddressWidth::ThreeByte,
        io_mode: IoMode::Single,
        dummy_cycles: 8,
        write_data: &[],
        read_buf: &mut buf,
    };

    m.execute(&mut cmd)
        .map_err(|e| format!("SPI error: {}", e))?;

    Ok(bytes_to_steel(&buf))
}

fn is_busy<M: SpiMaster>(master: &SharedMaster<M>) -> Result<bool, String> {
    let status = read_status(master, opcodes::RDSR)?;
    Ok((status & (opcodes::SR1_WIP as isize)) != 0)
}

fn wait_ready<M: SpiMaster>(master: &SharedMaster<M>, timeout_us: u32) -> Result<bool, String> {
    let poll_interval_us = 100;
    let max_polls = timeout_us / poll_interval_us;

    for _ in 0..max_polls {
        let status = read_status(master, opcodes::RDSR)?;
        if (status & (opcodes::SR1_WIP as isize)) == 0 {
            return Ok(true);
        }

        std::thread::sleep(std::time::Duration::from_micros(poll_interval_us as u64));
    }

    Err("timeout waiting for ready".to_string())
}

fn erase_block<M: SpiMaster>(
    master: &SharedMaster<M>,
    opcode: u8,
    addr: u32,
    use_4byte: bool,
) -> Result<bool, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    // Send WREN first
    let mut wren = SpiCommand::simple(opcodes::WREN);
    m.execute(&mut wren)
        .map_err(|e| format!("WREN error: {}", e))?;

    // Send erase command
    let mut cmd = if use_4byte {
        SpiCommand::erase_4b(opcode, addr)
    } else {
        SpiCommand::erase_3b(opcode, addr)
    };
    m.execute(&mut cmd)
        .map_err(|e| format!("erase error: {}", e))?;

    Ok(true)
}

fn page_program<M: SpiMaster>(
    master: &SharedMaster<M>,
    addr: u32,
    data: SteelVal,
    use_4byte: bool,
) -> Result<bool, String> {
    let bytes = steel_to_bytes(&data)?;

    // Validate page size (max 256 bytes for standard page program)
    if bytes.len() > 256 {
        return Err(format!(
            "page program data too large: {} bytes (max 256)",
            bytes.len()
        ));
    }

    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    // Send WREN first
    let mut wren = SpiCommand::simple(opcodes::WREN);
    m.execute(&mut wren)
        .map_err(|e| format!("WREN error: {}", e))?;

    // Send page program command
    let mut cmd = if use_4byte {
        SpiCommand::write_4b(opcodes::PP_4B, addr, &bytes)
    } else {
        SpiCommand::write_3b(opcodes::PP, addr, &bytes)
    };
    m.execute(&mut cmd)
        .map_err(|e| format!("page program error: {}", e))?;

    // Drop the lock before polling
    drop(m);

    // Wait for completion (typical page program time is 0.7-3ms, max ~5ms)
    wait_ready(master, 10_000)?;

    Ok(true)
}

fn chip_erase<M: SpiMaster>(master: &SharedMaster<M>) -> Result<bool, String> {
    let mut m = master.lock().map_err(|e| format!("lock error: {}", e))?;

    // Send WREN first
    let mut wren = SpiCommand::simple(opcodes::WREN);
    m.execute(&mut wren)
        .map_err(|e| format!("WREN error: {}", e))?;

    // Send chip erase command
    let mut cmd = SpiCommand::simple(opcodes::CE_C7);
    m.execute(&mut cmd)
        .map_err(|e| format!("chip erase error: {}", e))?;

    Ok(true)
}

fn print_help() {
    println!(
        r#"
rflasher Scheme REPL - Available Commands
==========================================

LOW-LEVEL SPI COMMANDS
----------------------
(spi-execute opcode addr dummy write-data read-len)
    Execute a raw SPI command. addr can be #f for no address.
    write-data can be #f for no data.

(spi-simple opcode)
    Execute a simple command with no address or data.

(spi-read-reg opcode len)
    Read a register (no address).

(spi-write-reg opcode data)
    Write a register (no address). data is a list of bytes.

(spi-read opcode addr len)
    Read len bytes from addr using 3-byte addressing.

(spi-read-4b opcode addr len)
    Read len bytes from addr using 4-byte addressing.

(spi-write opcode addr data)
    Write data to addr using 3-byte addressing.

(spi-write-4b opcode addr data)
    Write data to addr using 4-byte addressing.

MULTI-IO READS
--------------
(spi-read-dual-out opcode addr len)
    Read using Dual Output mode (1-1-2).

(spi-read-dual-io opcode addr len)
    Read using Dual I/O mode (1-2-2).

(spi-read-quad-out opcode addr len)
    Read using Quad Output mode (1-1-4).

(spi-read-quad-io opcode addr len)
    Read using Quad I/O mode (1-4-4).

HIGH-LEVEL HELPERS
------------------
(read-jedec-id)         Read JEDEC ID, returns (manufacturer device).
(read-status1)          Read status register 1.
(read-status2)          Read status register 2.
(read-status3)          Read status register 3.
(write-status1 value)   Write status register 1.
(write-status2 value)   Write status register 2.
(write-status3 value)   Write status register 3.
(write-enable)          Send Write Enable command.
(write-disable)         Send Write Disable command.
(is-busy?)              Check if WIP bit is set.
(wait-ready timeout-us) Wait for WIP to clear, with timeout.
(chip-erase)            Erase entire chip (DANGEROUS!).
(sector-erase addr)     Erase 4KB sector at addr.
(block-erase-32k addr)  Erase 32KB block at addr.
(block-erase-64k addr)  Erase 64KB block at addr.
(page-program addr data)     Program up to 256 bytes at addr (handles WREN + wait).
(page-program-4b addr data)  Same as page-program but with 4-byte addressing.
(enter-4byte-mode)      Enter 4-byte address mode.
(exit-4byte-mode)       Exit 4-byte address mode.
(reset-enable)          Send Reset Enable command.
(reset)                 Send Reset command.
(deep-power-down)       Enter deep power-down mode.
(release-power-down)    Release from deep power-down.
(read-sfdp addr len)    Read SFDP data.

BYTE UTILITIES
--------------
(make-bytes len fill)   Create a byte list of len bytes, all set to fill.
(random-bytes len)      Create a byte list of len random bytes.
(bytes-length data)     Return length of byte list.
(bytes-ref data idx)    Get byte at index.
(bytes->list data)      Convert to list.
(list->bytes list)      Convert list to bytes.
(bytes->hex data)       Convert to hex string.
(hex->bytes str)        Parse hex string to bytes.

SPI25 OPCODES (from rflasher/spi25 module)
------------------------------------------
WREN, WRDI, RDSR, RDSR2, RDSR3, WRSR, WRSR2, WRSR3,
RDID, READ, FAST_READ, READ_4B, PP, PP_4B,
SE_20, BE_52, BE_D8, CE_C7, EN4B, EX4B,
DOR, DIOR, QOR, QIOR (and 4B variants),
RDSFDP, RSTEN, RST, DP, RDP,
SR1_WIP, SR1_WEL, SR1_BP0, SR1_BP1, SR1_BP2, SR1_TB, SR1_SRP0,
SR2_QE, SR2_CMP, SR3_WPS, etc.

EXAMPLES
--------
; Read JEDEC ID
> (read-jedec-id)
(239 16404)  ; 0xEF 0x4014 = Winbond W25Q80

; Read first 16 bytes of flash
> (spi-read READ 0 16)
(255 255 255 255 255 255 255 255 255 255 255 255 255 255 255 255)

; Check if chip is busy
> (is-busy?)
#f

; Read status register and check write-protect bits
> (define sr1 (read-status1))
> (bitwise-and sr1 (+ SR1_BP0 SR1_BP1 SR1_BP2))
0

; Convert data to hex for display
> (bytes->hex (spi-read READ 0 8))
"ff ff ff ff ff ff ff ff"

; Generate random data and program a page (erase sector first!)
> (sector-erase #x1000)
> (wait-ready 1000000)
> (define data (random-bytes 256))
> (page-program #x1000 data)
#t
> (equal? data (spi-read READ #x1000 256))
#t

(quit) or (exit) to exit the REPL.
"#
    );
}
