# rflasher - Architecture and Implementation Plan

A modern Rust port of flashprog for reading, writing, and erasing flash chips.

## Design Goals

1. **Idiomatic Rust workspace** with clean separation of concerns
2. **`no_std` compatible core** for embedded use (including async with Embassy)
3. **YAML-based chip database** with build-time code generation
4. **Trait-based programmer abstraction** supporting SPI, Parallel, and Opaque masters
5. **New CLI** (not compatible with flashprog)
6. **Linkable library** for integration into other tools

---

## Current Implementation Status

### Phase 1: Foundation - COMPLETE

The core library and basic infrastructure are fully implemented:

- **Workspace Structure**: 8 crates properly configured
- **rflasher-core**: Complete `no_std` compatible core library
  - Error types (`error.rs`)
  - SPI types: `SpiCommand`, `IoMode`, `AddressWidth`, full JEDEC opcodes
  - Programmer traits: `SpiMaster`, `AsyncSpiMaster`, `OpaqueMaster`
  - Chip types: `FlashChip`, `EraseBlock`, `Features` bitflags
  - Protocol layer: SPI25 command implementations
  - Flash operations: probe/read/write/erase/verify
  - Write protection range decoding
- **rflasher-dummy**: Complete in-memory flash emulator with tests
- **CLI Binary**: Working clap-based CLI with all commands stubbed
  - `probe`, `read`, `write`, `erase`, `verify`, `info` commands
  - `list-programmers`, `list-chips` commands
  - Works with dummy programmer

### What Works Now

```bash
# List supported chips (hardcoded for now)
rflasher list-chips

# Probe for chip using dummy programmer
rflasher probe -p dummy

# Show chip info
rflasher info -p dummy

# List available programmers
rflasher list-programmers
```

---

## Repository Structure

```
rflasher/
├── Cargo.toml                      # Workspace definition
├── ARCHITECTURE.md                 # This file
│
├── chips/                          # RON flash chip database
│   └── vendors/
│       ├── winbond.ron             # Winbond W25Q/W25X series
│       ├── gigadevice.ron          # GigaDevice GD25Q/GD25LQ series
│       └── macronix.ron            # Macronix MX25L/MX25U/MX66 series
│
├── crates/
│   ├── rflasher-core/              # Core library (no_std) - COMPLETE
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── error.rs
│   │       ├── chip/
│   │       │   ├── mod.rs
│   │       │   ├── types.rs        # FlashChip, EraseBlock, etc.
│   │       │   └── features.rs     # Feature bitflags
│   │       ├── spi/
│   │       │   ├── mod.rs
│   │       │   ├── command.rs      # SpiCommand struct
│   │       │   ├── opcodes.rs      # JEDEC SPI opcodes
│   │       │   ├── io_mode.rs      # Single/Dual/Quad/QPI
│   │       │   └── address.rs      # 3-byte vs 4-byte addressing
│   │       ├── programmer/
│   │       │   ├── mod.rs
│   │       │   └── traits.rs       # SpiMaster, OpaqueMaster traits
│   │       ├── protocol/
│   │       │   ├── mod.rs
│   │       │   └── spi25.rs        # SPI25 command sequences
│   │       ├── flash/
│   │       │   ├── mod.rs
│   │       │   ├── context.rs      # FlashContext runtime state
│   │       │   └── operations.rs   # High-level read/write/erase
│   │       └── wp/
│   │           ├── mod.rs
│   │           └── ranges.rs       # Write protection decoding
│   │
│   ├── rflasher-chips-codegen/     # Build-time code generator - COMPLETE
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # RON parsing, validation, codegen
│   │
│   ├── rflasher-ch341a/            # CH341A USB programmer - STUB
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   │
│   ├── rflasher-serprog/           # Serial Flasher Protocol - STUB
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   │
│   ├── rflasher-ftdi/              # FT2232H/FT232H/FT4232H - STUB
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   │
│   ├── rflasher-internal/          # Intel chipset internal - STUB
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   │
│   ├── rflasher-linux-spi/         # Linux spidev - STUB
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   │
│   └── rflasher-dummy/             # Dummy/emulator for testing - COMPLETE
│       ├── Cargo.toml
│       └── src/lib.rs
│
├── src/                            # CLI binary - COMPLETE (basic)
│   ├── main.rs
│   ├── cli.rs                      # clap derive-based argument parsing
│   └── commands/
│       ├── mod.rs
│       ├── probe.rs
│       └── list.rs
│
└── flashprog/                      # Reference C implementation (submodule)
```

---

## Remaining Implementation Phases

### Phase 2: Chip Database - COMPLETE

**Goal**: RON-based chip database with build-time codegen

**Implemented**:
1. `rflasher-chips-codegen` crate with:
   - RON parsing with serde
   - Chip database validation
   - Rust code generation
2. RON files for common chips (57 chips total):
   - Winbond W25Q/W25X series (24 chips)
   - GigaDevice GD25Q/GD25LQ series (15 chips)
   - Macronix MX25L/MX25U/MX25R/MX66 series (18 chips)
3. Build-time codegen via `build.rs` in rflasher-core
4. Generated chip database replaces hardcoded chips

**Reference**: `flashprog/flashchips.c` contains ~600 chip definitions (more can be ported)

### Phase 3: Complete CLI Commands - COMPLETE

**Goal**: Fully functional read/write/erase/verify commands

**Implemented**:
1. `read` command:
   - File I/O with progress bar (indicatif)
   - Chunked reading (4 KiB chunks)
2. `write` command:
   - Read file, erase, program, verify cycle
   - Progress reporting for each phase
   - Optional verification (`--verify`)
   - Optional skip erase (`--no-erase`)
3. `erase` command:
   - Full chip erase
   - Sector-level erase with `--start` and `--length` options
4. `verify` command:
   - Compare file against flash contents
   - Reports first mismatch location and total mismatch count
5. Progress bars using `indicatif` crate for all operations

**Usage Examples**:
```bash
# Read flash to file
rflasher read -p dummy -o flash.bin

# Write file to flash (with erase and verify)
rflasher write -p dummy -i flash.bin

# Write without erasing
rflasher write -p dummy -i flash.bin --no-erase

# Erase entire chip
rflasher erase -p dummy

# Erase 64 KiB starting at 0x10000
rflasher erase -p dummy --start 0x10000 --length 0x10000

# Verify flash against file
rflasher verify -p dummy -i flash.bin
```

### Phase 4: CH341A Programmer - COMPLETE

**Goal**: Working CH341A USB programmer

**Implemented**:
1. USB communication using `nusb` crate with blocking I/O via `futures-lite`
2. Full protocol implementation from `flashprog/ch341a_spi.c`:
   - VID: 0x1A86, PID: 0x5512
   - Bulk transfers for SPI streaming
   - CS control via UIO stream commands
   - Bit reversal on data bytes (lookup table)
   - Delay accumulation for CS timing
3. `SpiMaster` trait implementation with 4KB read/write support
4. Device detection, initialization, and enumeration
5. Comprehensive error handling for USB conditions

**Crate Structure** (`rflasher-ch341a`):
```
src/
├── lib.rs       # Public exports
├── device.rs    # Ch341a struct and SpiMaster impl
├── protocol.rs  # USB protocol constants
└── error.rs     # Error types
```

**Usage Examples**:
```bash
# Probe for chip using CH341A
rflasher probe -p ch341a

# Read flash to file
rflasher read -p ch341a -o flash.bin

# Write file to flash
rflasher write -p ch341a -i flash.bin
```

### Phase 5: Serprog Programmer - COMPLETE

**Goal**: Serial Flasher Protocol support

**Implemented**:
1. Full protocol implementation from `flashprog/serprog.c`:
   - Protocol synchronization (SYNCNOP)
   - Interface version query (Q_IFACE)
   - Command map query (Q_CMDMAP)
   - Bus type query and setting (Q_BUSTYPE, S_BUSTYPE)
   - Programmer name query (Q_PGMNAME)
   - SPI operation command (O_SPIOP)
   - SPI frequency setting (S_SPI_FREQ)
   - Chip select setting (S_SPI_CS)
   - Pin state control (S_PIN_STATE)
2. Serial port backend using `serialport` crate
3. TCP socket backend for network-attached programmers
4. `SpiMaster` trait implementation
5. Programmer string parsing with options

**Crate Structure** (`rflasher-serprog`):
```
src/
├── lib.rs       # Public exports, convenience functions
├── device.rs    # Serprog struct and SpiMaster impl
├── protocol.rs  # Protocol constants and types
├── transport.rs # Serial and TCP transport backends
└── error.rs     # Error types
```

**Usage Examples**:
```bash
# Probe via serial port (default baud rate)
rflasher probe -p serprog:dev=/dev/ttyUSB0

# Probe via serial port with specific baud rate
rflasher probe -p serprog:dev=/dev/ttyUSB0:115200

# Probe via TCP (e.g., ESP8266 serprog)
rflasher probe -p serprog:ip=192.168.1.100:5000

# Read flash with SPI speed setting
rflasher read -p serprog:dev=/dev/ttyUSB0,spispeed=2000000 -o flash.bin

# Write with specific chip select
rflasher write -p serprog:dev=/dev/ttyUSB0,cs=1 -i flash.bin
```

### Phase 6: FTDI Programmer

**Goal**: FTDI MPSSE programmer support

**Tasks**:
1. Use `libftd2xx` or `ftdi-rs` crate
2. Implement MPSSE mode for SPI
3. Support multiple device types:
   - FT2232H (dual channel)
   - FT4232H (quad channel)
   - FT232H (single channel)
4. GPIO control for chip select
5. Port from `flashprog/ft2232_spi.c`

### Phase 7: Linux SPI

**Goal**: Linux spidev support

**Tasks**:
1. Implement using `/dev/spidevX.Y` interface
2. Use `spidev` crate or raw ioctl
3. Support configurable:
   - SPI mode (0-3)
   - Clock speed
   - Bits per word
4. Port from `flashprog/linux_spi.c`

### Phase 8: Intel Internal Programmer

**Goal**: Intel chipset internal flash support

**Tasks**:
1. PCI device detection
2. Memory-mapped I/O for register access
3. Implement ICH/PCH SPI controller support
4. Parse Intel Flash Descriptor (IFD)
5. Handle flash regions (BIOS, ME, GbE, etc.)
6. Implement hardware sequencing mode
7. Port from `flashprog/ichspi.c`

**Note**: This is an `OpaqueMaster` implementation, not `SpiMaster`

### Phase 9: Layout Support - COMPLETE

**Goal**: Flash layout support for region-based operations

**Implemented**:
1. TOML-based layout file format with:
   - Named regions with start/end addresses
   - `readonly` and `dangerous` flags per region
   - Optional chip size validation
2. Intel Flash Descriptor (IFD) parsing:
   - Automatic detection via signature (0x0FF0A55A)
   - Extracts region names (descriptor, bios, me, gbe, etc.)
   - Marks dangerous regions (ME, descriptor)
3. FMAP parsing (Chromebook-style):
   - Signature search ("__FMAP__")
   - Extracts regions with names and flags
   - Supports version 1.x
4. Layout CLI commands:
   - `layout show` - Display layout from TOML file
   - `layout extract` - Auto-detect and extract IFD/FMAP
   - `layout ifd` - Extract IFD specifically
   - `layout fmap` - Extract FMAP specifically
   - `layout create` - Create template layout file
5. Layout options on read/write/erase/verify:
   - `--layout <file>` - Use layout file
   - `--include <regions>` - Include specific regions
   - `--exclude <regions>` - Exclude regions
   - `--region <name>` - Single region shorthand

**Layout File Format (TOML)**:
```toml
[layout]
name = "My BIOS"
chip_size = "16 MiB"

[[region]]
name = "descriptor"
start = 0x000000
end = 0x000FFF
readonly = true

[[region]]
name = "bios"
start = 0x001000
end = 0x7FFFFF

[[region]]
name = "me"
start = 0x800000
end = 0xFFFFFF
dangerous = true
```

### Phase 10: Remaining Features

**Goal**: Production-ready tool

**Tasks**:
1. Optimal erase algorithm (minimize erase operations)
2. Write protection management
3. SFDP parsing for auto-detection
4. Comprehensive test suite

---

## Key Implementation Details

### Programmer Traits (Already Implemented)

```rust
pub trait SpiMaster {
    fn features(&self) -> SpiFeatures;
    fn max_read_len(&self) -> usize;
    fn max_write_len(&self) -> usize;
    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()>;
    fn probe_opcode(&self, opcode: u8) -> bool { true }
    fn delay_us(&mut self, us: u32);
}

pub trait OpaqueMaster {
    fn size(&self) -> usize;
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()>;
    fn erase(&mut self, addr: u32, len: u32) -> Result<()>;
}
```

### RON Chip Schema

```ron
// chips/vendors/winbond.ron
(
    vendor: "Winbond",
    manufacturer_id: 0xEF,
    chips: [
        (
            name: "W25Q128.V",
            device_id: 0x4018,
            total_size: MiB(16),  // Human-readable sizes: B(n), KiB(n), MiB(n)
            features: (
                wrsr_wren: true,
                fast_read: true,
                dual_io: true,
                quad_io: true,
                otp: true,
                erase_4k: true,
                erase_32k: true,
                erase_64k: true,
                status_reg_2: true,
                qe_sr2: true,
                wp_tb: true,
                wp_sec: true,
                wp_cmp: true,
            ),
            voltage: (min: 2700, max: 3600),
            erase_blocks: [
                (opcode: 0x20, size: KiB(4)),
                (opcode: 0x52, size: KiB(32)),
                (opcode: 0xD8, size: KiB(64)),
                (opcode: 0x60, size: MiB(16)),
                (opcode: 0xC7, size: MiB(16)),
            ],
            tested: (probe: Ok, read: Ok, erase: Ok, write: Ok, wp: Ok),
        ),
    ],
)
```

**Schema features:**
- `Size` enum: `B(n)`, `KiB(n)`, `MiB(n)` for human-readable sizes
- `features` struct with bool fields instead of string array
- Code generation uses `quote` + `prettyplease` for clean output

---

## Reference Material

The `flashprog/` directory contains the reference C implementation:

| File | Purpose | Port To |
|------|---------|---------|
| `flashchips.c` | Chip database (~600 chips) | YAML files |
| `ch341a_spi.c` | CH341A protocol | `rflasher-ch341a` |
| `serprog.c` | Serprog protocol | `rflasher-serprog` |
| `ft2232_spi.c` | FTDI MPSSE | `rflasher-ftdi` |
| `linux_spi.c` | Linux spidev | `rflasher-linux-spi` |
| `ichspi.c` | Intel ICH/PCH | `rflasher-internal` |
| `spi25.c` | SPI commands | `rflasher-core/protocol` |
| `spi25_statusreg.c` | Status registers | `rflasher-core/protocol` |

---

## Safety Considerations

1. **Destructive operations**: Erase and write can brick devices
2. **Voltage mismatches**: Document voltage requirements
3. **Intel ME region**: Writing to ME region can brick the system
4. **Write protection**: Always check WP status before writing

---

## License

This project is licensed under GPL-2.0-or-later, same as flashprog.
