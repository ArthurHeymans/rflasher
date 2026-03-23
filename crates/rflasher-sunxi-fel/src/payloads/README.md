# SPI Payload Binaries

Pre-compiled SPI driver payloads that run on Allwinner SoCs via FEL.
Each `.bin` file is uploaded to the SoC's SRAM and executed to drive the
SPI controller through a bytecode command interface.

## Origin

These payloads are from the [xfel](https://github.com/xboot/xfel) project
by Jianjun Jiang, licensed under MIT. The source code for each payload is
kept alongside the binary in a `*_spi_src/` directory.

## Rebuilding

The D1/F133 payload requires a RISC-V toolchain. xfel uses the
Xuantie/T-Head toolchain:

```sh
# Download toolchain (or use any riscv64 GCC)
# https://occ.t-head.cn/community/download?id=4046947553902661632

cd d1_f133_spi_src
CROSS=riscv64-unknown-elf- make
cp output/spi.bin ../d1_f133.bin
```

The Makefile compiles `start.S` (entry point) and `sys-spi.c` (SPI
controller driver + bytecode interpreter) into a flat binary linked
at `0x00020000`.

## Adding a new SoC

1. Copy the payload source from `xfel/payloads/<chip>/spi/` into a new
   `<chip>_spi_src/` directory here
2. Build it with the appropriate cross-compiler (ARM for most SoCs,
   RISC-V for D1/F133)
3. Place the output binary as `<chip>.bin`
4. Add the chip to `chips.rs`: detection ID, payload reference via
   `include_bytes!()`, and memory layout addresses

## Memory Layout

Each payload has a fixed memory layout on the target SoC:

| Region      | D1/F133 Address | Size    | Purpose                           |
|-------------|-----------------|---------|-----------------------------------|
| Payload     | `0x00020000`    | ~1.5 KB | Compiled payload code             |
| Command buf | `0x00021000`    | 4 KB    | Bytecode commands from host       |
| Swap buf    | `0x00022000`    | 64 KB   | TX/RX data exchanged with host    |

## Bytecode Commands

The payload interprets these commands from the command buffer:

| Code | Name            | Args                  | Description                        |
|------|-----------------|-----------------------|------------------------------------|
| 0x00 | `END`           | none                  | Stop processing, return to FEL     |
| 0x01 | `INIT`          | none                  | Initialize clocks, GPIO, SPI       |
| 0x02 | `SELECT`        | none                  | Assert chip select (CS low)        |
| 0x03 | `DESELECT`      | none                  | Deassert chip select (CS high)     |
| 0x04 | `FAST`          | len:u8, data[len]     | Send bytes from command buffer     |
| 0x05 | `TXBUF`         | addr:u32le, len:u32le | Send bytes from SRAM address       |
| 0x06 | `RXBUF`         | addr:u32le, len:u32le | Receive bytes to SRAM address      |
| 0x07 | `SPINOR_WAIT`   | none                  | Poll RDSR until WIP clears         |
