# ATI/AMD GPU VBIOS SPI programmer

`ati_spi` is an experimental Linux-only backend, enabled with `--features
ati-spi` or `--features all-programmers`. It is not enabled by default.

```sh
cargo build --release --no-default-features --features ati-spi
sudo target/release/rflasher probe -p ati_spi:pci=0000:03:00.0
sudo target/release/rflasher read -p ati_spi:pci=0000:03:00.0 backup.rom
```

`pci=` is the full hexadecimal domain:bus:device.function address from `lspci
-Dnn`. Without it, exactly one matching GPU must exist; multiple matches are
an error. The CLI and REPL use the same selection and ownership checks.

## Prepare the GPU before even probing

Use a **secondary, idle GPU**, not the device driving your display or console.
Stop software using the GPU and have another display/remote console available.
The backend changes GPU registers even for a read or chip probe. It does not
unbind drivers automatically.

As root, after verifying the exact PCI address:

1. Record the current driver, `driver_override`, and `power/control` values in
   `/sys/bus/pci/devices/DDDD:BB:DD.F/` so you can restore them later.
2. Write `on` to that device's `power/control` and ensure `power_state` is `D0`.
3. Write `none` to its `driver_override` **before** unbinding its kernel driver.
   This disables normal driver matching during flashing. Leave it set for the
   entire session.
4. Write the BDF to the current driver's `unbind` attribute. Confirm the device's
   `driver` symlink is gone and the device remains powered in `D0`.

The backend refuses bound devices or devices without `driver_override=none`.
It also holds an advisory lock on the PCI config file until GPU registers are
restored and the BAR is unmapped, excluding other rflasher controller instances.
This is not exclusion against unrelated raw-MMIO tools, firmware, or an
administrator changing the binding: do not run another flasher, reset/hotplug
the GPU, or change its driver/power settings during the session.

After rflasher exits normally, restore the original `driver_override` (write a
newline to clear it if previously unset), reprobe the device through
`/sys/bus/pci/drivers_probe`, and restore the original power policy. Rflasher
does not change these sysfs settings itself. Killing it or losing power can
prevent register restoration; recover/reset the GPU before rebinding a driver.

## Flashing and recovery

Read twice and compare the complete dumps before attempting a write. If
possible, compare with a dump from a known-working vendor tool as well. Keep
an untouched backup on another device, and have a working dual-BIOS switch or
external SPI programmer for recovery. Use a VBIOS intended for the exact board.

```sh
sudo target/release/rflasher read -p ati_spi:pci=0000:03:00.0 backup2.rom
cmp backup.rom backup2.rom
sudo target/release/rflasher write -p ati_spi:pci=0000:03:00.0 new.rom
```

Writes verify by default. Software checks and successful unit tests do not
establish that a particular board can be safely erased/programmed.

## Coverage and transaction limits

- Implemented engines: R600/RV7xx, Evergreen, Northern Islands, Lombok, and
  Bonaire/Hawaii/Iceland through Polaris.
- Other Southern Islands (Tahiti, Pitcairn, Verde, Oland, Hainan) remain in the
  identification table but are excluded from selection and initialization:
  their flash enable/access sequence is not implemented. Kernel PCI IDs alone
  do not establish SPI compatibility.
- All Rust-port hardware support remains experimental. `TestStatus::Ok` in
  the table records historical **flashrom** tests, not tests of this port.
- Only single-I/O, byte-aligned transactions with no address or a three-byte
  address are supported. Four-byte addressing and simultaneous transmit-data
  and receive-data phases are refused before MMIO.
- SFDP and RDUID dummy clocks are received and discarded before returning the
  response. Reads reserve FIFO space for dummy bytes; page programs can still
  send a full 256-byte page.

## Prior implementations and reports

- [Ard Biesheuvel's Polaris VBIOS read/reflash walkthrough](https://www.workofard.com/2020/12/aarch64-option-roms-for-amd-gpus/).
- [Polaris 10/20 test report in the flashrom review](https://review.coreboot.org/c/flashrom/+/29219).
- [Luc Verhaegen's original reverse-engineering account](https://libv.livejournal.com/28024.html).
- [Original flashrom ATI driver](https://github.com/ardbiesheuvel/flashrom/blob/8e75e0b126ad6f2c13c47a1f4b508f8b99ee329c/ati_spi.c).
- [Linux PCI sysfs binding documentation](https://www.kernel.org/doc/Documentation/ABI/testing/sysfs-bus-pci).

These support the underlying approach, not a claim that this Rust port has
passed hardware testing. The original flashrom series was abandoned and is
not upstream support.
