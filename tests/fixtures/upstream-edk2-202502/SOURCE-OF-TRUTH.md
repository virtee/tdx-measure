# `upstream-edk2-202502` fixture source of truth

This fixture records the ACPI tables captured from a live Intel TDX guest and the configuration and expected measurements used to test them.
The captured `acpi_tables.bin`, test configuration `metadata.json`, and measurement expectations `expected.json` are committed to the repository.
The required firmware binary, `OVMF-edk2-stable202502.fd`, is not committed and must be built locally before using the fixture.

`acpi_tables.bin` contains the raw QEMU `fw_cfg` ACPI tables payload extracted from a live Intel TDX guest.
It was captured by running [`extract_config_files.py`](../../../extract_config_files.py) inside the guest and copying out `/sys/firmware/qemu_fw_cfg/by_name/etc/acpi/tables/raw`.

The presence of this `SOURCE-OF-TRUTH.md` file marks the `acpi_tables.bin` as runtime-captured: `tests/regen_fixtures.sh` will NOT overwrite `acpi_tables.bin` when refreshing the fixture.
Instead, the script refreshes the expected MRTD and RTMR0 values in `expected.json`, which `tests/parse.rs` checks.
The optional `tests/regen.rs` integration test re-runs `--create-acpi-tables` and asserts byte-equality against this captured blob.
To replace it, capture a fresh `acpi_tables.bin` from another guest; do NOT regenerate the reference via `tdx-measure --create-acpi-tables`.

The accompanying `metadata.json` names the fixture-relative firmware and ACPI inputs and records the configuration used by the parse/regen tests.
Its QEMU device list is retained from the existing regeneration configuration (`virtio-rng-pci`), not the capture command's cloud-init `virtio-blk-pci` device.

## Prepare the firmware before using this fixture

Run the following command from the repository root before running measurement tests, checking ACPI regeneration, refreshing expectations, or using the QEMU command below:

```bash
tests/fetch_ovmf.sh
```

The script requires `jq` and Docker for the versioned firmware build.
Its default mode also downloads the shared Ubuntu firmware used by other fixtures, requiring `wget` and `dpkg-deb`.
It builds this fixture's TDX-capable UEFI firmware from pinned EDK II sources for `edk2-stable202502` and writes it to `tests/fixtures/upstream-edk2-202502/OVMF-edk2-stable202502.fd`.
The firmware is built locally, not extracted from the capture guest or supplied as a committed binary.

The measurement tests use the firmware contents to calculate MRTD and the firmware-related contributions to RTMR0, so the binary must match the firmware used for the capture.
The script verifies the built firmware against a pinned SHA-256 digest and reuses an existing binary only when its digest and recorded build inputs match.

## Guest configuration used for the capture

The ACPI table fixture was captured from an Ubuntu 26.04 Server TDX guest with 32 vCPUs and 16 GiB RAM, using KVM acceleration and host CPU passthrough.

### Firmware and guest images

- Firmware: `OVMF-edk2-stable202502.fd`, built locally by `tests/fetch_ovmf.sh` as described above, rather than included in the repository.
- Guest disk: [`ubuntu-26.04-server-cloudimg-amd64.img`](https://cloud-images.ubuntu.com/releases/resolute/release-20261016/ubuntu-26.04-server-cloudimg-amd64.img) from Canonical's dated `release-20261016` build.
  - The dated release identifies the image used for this capture rather than Canonical's changing `current` image.
    If that release becomes unavailable, the committed `acpi_tables.bin` remains the authoritative ACPI reference for the tests.
- Guest image preparation and creation of the cloud-init disk `user-data.img` followed the [Ubuntu Server TD guest image guide](https://ubuntu.com/server/docs/how-to/virtualisation/intel-tdx/#create-a-guest-image).
- `vmlinuz-7.0.0-38-generic` and `initrd.img-7.0.0-38-generic` were extracted from that guest disk using `sudo python3 extract_kernel_initrd.py ubuntu-26.04-server-cloudimg-amd64.img`, following the **Ubuntu 26.04** kernel section of the root [README](../../../README.md).

The extractor attaches the image through NBD and mounts its partitions read-only; both extracted files come from the guest image, not the host.


### QEMU launch command

The full command is included for reference, with paths relative to the repository root.
After the firmware preparation step, the locally built firmware is located in the fixture folder.
The kernel, initrd, guest disk, and cloud-init disk images must be prepared separately and placed in the repository root.
The kernel and initrd are needed to boot the TD and extract its ACPI tables, but are not inputs to the fixture's MRTD and RTMR0 calculations.

```bash
qemu-system-x86_64 -accel kvm \
  -m 16G -smp 32 \
  -cpu host \
  -object '{"qom-type":"tdx-guest","id":"tdx","quote-generation-socket":{"type":"vsock","cid":"2","port":"4050"}}' \
  -object memory-backend-ram,id=mem0,size=16G \
  -machine q35,kernel_irqchip=split,confidential-guest-support=tdx,memory-backend=mem0 \
  -bios tests/fixtures/upstream-edk2-202502/OVMF-edk2-stable202502.fd \
  -nographic \
  -nodefaults \
  -vga none \
  -kernel vmlinuz-7.0.0-38-generic \
  -initrd initrd.img-7.0.0-38-generic \
  -append "root=/dev/sda1 console=ttyS0" \
  -hda ubuntu-26.04-server-cloudimg-amd64.img \
  -drive file=user-data.img,if=none,id=cloud-init,format=raw \
  -device virtio-blk-pci,drive=cloud-init \
  -serial stdio
```

- `q35,kernel_irqchip=split` selects the Q35 machine with a split interrupt controller implementation.
- `confidential-guest-support=tdx` associates the machine with the `tdx-guest` object, and `memory-backend=mem0` selects its 16 GiB RAM backend.
- The TDX object's quote-generation socket points to a service at vsock CID 2, port 4050.
- `-nodefaults` disables automatic default devices, `-vga none` disables VGA, and `-nographic` with `-serial stdio` provides a terminal-based serial console.
- `-kernel`, `-initrd`, and `-append` select direct kernel boot, with `/dev/sda1` as the root filesystem and `ttyS0` as the kernel console.
- `-hda` attaches the guest system disk, while the cloud-init disk is attached separately as a raw image through `virtio-blk-pci`.
