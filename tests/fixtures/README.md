# tdx-measure test fixtures

Each subdirectory here is one self-contained test fixture covering a different
QEMU shape. The integration tests at `tests/parse.rs` walk every directory and
re-run the measurement pipeline against the bytes inside. The (slower, gated)
tests at `tests/regen.rs` additionally drive `--create-acpi-tables` end-to-end
to make sure the dumper still produces those exact bytes.

## On-disk layout

```
tests/fixtures/
├── OVMF.fd                      # shared firmware blob; fetched locally, not committed
├── canonical-defaults/
│   ├── metadata.json            # input config (cpu / memory / qemu block / ...)
│   ├── acpi_tables.bin          # the ACPI blob the tool produces for this shape
│   └── expected.json            # { mrtd, rtmr0 } expected from --platform-only
├── q35-with-hpet/
│   └── … (same layout)
├── upstream-edk2-202502/
│   ├── OVMF-edk2-stable202502.fd # versioned firmware; built locally, not committed
│   └── … (same layout)
└── README.md                    # (this file)
```

`metadata.json` is the fixture input.
During integration tests, paths in `boot_config.bios` and `boot_config.acpi_tables` are resolved relative to the fixture directory.
The shared `tests/fixtures/OVMF.fd` file is downloaded by `tests/fetch_ovmf.sh` from the pinned Ubuntu OVMF package.
It is intentionally not committed to the repository.
The `upstream-edk2-202502` fixture uses a separate firmware file, `OVMF-edk2-stable202502.fd`.
`tests/fetch_ovmf.sh` builds this file with `tests/Dockerfile.edk2-ovmf-build` from the pinned EDK2 `edk2-stable202502` commit.
The build also pins its Ubuntu package snapshot and Debian patch archive, and verifies the resulting firmware SHA-256 digest.
Both firmware files are local test prerequisites.
They are created or validated by `tests/fetch_ovmf.sh`.
The test suite does not build them automatically.
Run that script once after checking out the repository, or again whenever its pinned inputs change.

`acpi_tables.bin` and `expected.json` are the captured ground truth. They
should change only when the underlying QEMU pin in `src/acpi.rs` changes, or
when the fixture's `metadata.json` shape changes (in which case re-run
`tests/regen_fixtures.sh` to refresh both).

## Current shapes

The fixtures fall into two flavours depending on where the ground-truth bytes
in `acpi_tables.bin` came from:

- **tool-generated** — `acpi_tables.bin` is what `tdx-measure --create-acpi-tables`
  produced for the configuration in `metadata.json`. The parse test catches
  measurement-pipeline drift (different RTMR0 from the same input bytes); the
  regen test catches dumper self-inconsistency. Both fixtures here move
  together if the dumper has a latent bug, so they don't prove correctness
  against an external oracle.

- **runtime-captured** — `acpi_tables.bin` was extracted from a live TD via
  the in-guest `extract_config_files.py` script (reads
  `/sys/firmware/qemu_fw_cfg/by_name/etc/acpi/tables/raw`). These bytes are
  what the firmware actually saw; the regen test against them proves that the
  patched-QEMU dumper produces the same output as a runtime TD would.

| Fixture | Source | Coverage |
|---|---|---|
| `canonical-defaults` | tool-generated | The Canonical direct-boot args used when no `boot_config.qemu` block is present (`hpet=off,smm=off,pic=off`, `-cpu host -accel kvm`). Pins the Canonical-defaults reference RTMR0. |
| `q35-with-hpet` | tool-generated | A generic q35 with HPET on, a handful of `virtio-*-pci` devices, and `accel: "tcg"` so the regen test runs without KVM. Exercises the larger-table-set path of `derive_table_loader` (HPET adds an extra `AddChecksum` + an extra RSDT pointer). |
| `runtime-capture` | runtime-captured | Bytes extracted from a live TDX guest (Tinfoil cvmimage, 8 vCPU / 16 GiB, q35 with HPET and a typical set of `virtio-scsi` / `e1000` / `vhost-vsock` devices). The regen test against this fixture is the only one that catches drift between our patched-QEMU dumper and what an actual TD exposes at runtime. |
| `upstream-edk2-202502` | runtime-captured | Bytes extracted from a live Intel TDX guest (32 vCPU / 16 GiB, q35 with `kernel_irqchip=split`) booted with `-bios OVMF-edk2-stable202502.fd`; see `SOURCE-OF-TRUTH.md` for the exact QEMU command. `acpi_tables.bin` was extracted in-guest via `extract_config_files.py`. Exercises MRTD/CFV measurement against a real OVMF binary built from upstream EDK2. |

Adding a new tool-generated fixture: `mkdir tests/fixtures/<name>/` + a new
`metadata.json`, then `tests/regen_fixtures.sh` populates the rest.

Adding a new runtime-captured fixture: drop in the externally-extracted
`acpi_tables.bin` first, then write `metadata.json` describing the QEMU shape
that produced it. `tests/regen_fixtures.sh` will then verify the dumper's
round-trip on it (i.e. the regen output must equal the externally-captured
bytes); if you change the dumper and the runtime-captured fixture starts
failing, that's the dumper drifting from real-TD behaviour.

## Refresh / setup

```bash
# One-time on a fresh checkout (needs jq; also builds the versioned upstream-edk2 fixtures via Docker):
./tests/fetch_ovmf.sh

# When the QEMU pin or a fixture's metadata.json changes (needs jq + Docker):
./tests/regen_fixtures.sh

# Fast tests (any host, once the fixtures above are fetched/built):
cargo test --release --test parse

# Slow tests (needs Docker + buildx; KVM optional, only used by canonical-defaults):
cargo test --release --test regen -- --ignored --nocapture
```
