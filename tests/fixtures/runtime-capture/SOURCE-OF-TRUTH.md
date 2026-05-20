# `runtime-capture/acpi_tables.bin` source of truth

These bytes were extracted from a live TDX guest by running
[`extract_config_files.py`](../../../extract_config_files.py) inside the guest
and copying out `/sys/firmware/qemu_fw_cfg/by_name/etc/acpi/tables/raw`.

The presence of this `SOURCE-OF-TRUTH.md` file marks the fixture as runtime-
captured: `tests/regen_fixtures.sh` will NOT overwrite `acpi_tables.bin`
when refreshing the fixture, so the file stays authoritative even if the
dumper changes. The `tests/regen.rs` integration test then re-runs
`--create-acpi-tables` and asserts byte-equality against this blob, which is
the only fixture-level check that catches drift between our patched-QEMU
dumper and a real TD's `etc/acpi/tables`.

To replace these bytes, capture a fresh `acpi_tables.bin` from another guest
and drop it here in place; do NOT regenerate via `tdx-measure --create-acpi-tables`.

## Guest configuration that produced these bytes

- Tinfoil cvmimage, QEMU 10.2.1, 8 vCPUs, 16 GiB RAM
- `q35,kernel_irqchip=split,smm=off,pic=off,confidential-guest-support=tdx`
- HPET enabled (the default; tinfoild's launch line does not pass `hpet=off`)
- Five user PCI devices on `pcie.0`: e1000 (slot 2), vhost-vsock-pci, three
  virtio-scsi-pci controllers (auto-assigned to slots 1/3/4/5)

The accompanying `metadata.json` re-encodes that shape with `accel: "tcg"` and
`cpu: "Skylake-Server,phys-bits=46"` so the regen test runs on KVM-less hosts.
