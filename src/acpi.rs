/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * Copyright (c) 2025-2026 Intel Corporation
 * SPDX-License-Identifier: Apache-2.0
 */
//! This module provides functionality to load ACPI tables for QEMU from files.

use anyhow::{anyhow, bail, Context, Result};
use log::{info, warn};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util::read_file_data;
use crate::{ImageConfig, Machine, QemuShape};

const DOCKERFILE_QEMU_ACPI_DUMP: &str = include_str!("../Dockerfile.qemu-acpi-dump");
const CONTAINER_NAME: &str = "acpi-tables-generator";
const IMAGE_NAME: &str = "acpi-tables-generator";
const OVMF_IN_CONTAINER: &str = "/usr/share/ovmf/OVMF.fd";

const LDR_LENGTH: usize = 4096;
const FIXED_STRING_LEN: usize = 56;

pub struct Tables {
    pub tables: Vec<u8>,
    pub rsdp: Vec<u8>,
    pub loader: Vec<u8>,
}

impl Machine<'_> {
    pub fn build_tables(&self) -> Result<Tables> {
        if self.direct_boot && self.create_acpi_table {
            generate_acpi_tables(self.metadata_path, self.distribution, self.qemu_version)?;
        }

        let tables  = read_file_data(self.acpi_tables)?;

        let rsdp: Vec<u8> = if !self.rsdp.is_empty() {
            read_file_data(self.rsdp)?
        } else {
            let (rsdt_offset, _rsdt_csum, _rsdt_len) = find_acpi_table(&tables , "RSDT")?;

            // Generate RSDP
            let mut rsdp = Vec::with_capacity(20);
            rsdp.extend_from_slice(b"RSD PTR "); // Signature
            rsdp.push(0x00); // Checksum placeholder
            rsdp.extend_from_slice(b"BOCHS "); // OEM ID
            rsdp.push(0x00); // Revision
            rsdp.extend_from_slice(&rsdt_offset.to_le_bytes()); // RSDT Address
            rsdp
        };

        let loader: Vec<u8> = if !self.table_loader.is_empty() {
            read_file_data(self.table_loader)?
        } else {
            derive_table_loader(&tables)?
        };

        Ok(Tables {
            tables,
            rsdp,
            loader,
        })
    }
}

/// Walks the concatenated ACPI tables blob exposed by QEMU via `etc/acpi/tables`
/// and returns one entry per System Description Table found in it, in file order.
/// Each tuple is `(signature, offset, csum_offset, length)` where `csum_offset`
/// is the byte offset of the table's `Checksum` field (header offset 9).
fn list_acpi_tables(tables: &[u8]) -> Result<Vec<([u8; 4], u32, u32, u32)>> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 8 <= tables.len() {
        let sig: [u8; 4] = tables[off..off + 4].try_into().unwrap();
        if !sig.iter().all(|b| (32..127).contains(b)) {
            break;
        }
        let len = u32::from_le_bytes(tables[off + 4..off + 8].try_into().unwrap()) as usize;
        if len < 8 || off + len > tables.len() {
            bail!("ACPI table at offset {off:#x} has invalid length {len}");
        }
        out.push((sig, off as u32, (off + 9) as u32, len as u32));
        off += len;
    }
    Ok(out)
}

/// Build the QEMU table-loader blob (`etc/table-loader`) from a concatenated ACPI
/// tables image. The command order mirrors what QEMU's `acpi_build` emits today:
///   Allocate rsdp + Allocate tables
///   AddChecksum DSDT
///   AddPtr FACP→FACS, FACP→DSDT (4-byte), FACP→DSDT (8-byte X_DSDT)
///   AddChecksum FACP
///   AddChecksum {APIC, HPET, MCFG, WAET, ...} in file order
///   AddPtr RSDT→entry_i for each 4-byte entry (one per non-DSDT table)
///   AddChecksum RSDT
///   AddPtr RSDP→RSDT, AddChecksum RSDP
fn derive_table_loader(tables: &[u8]) -> Result<Vec<u8>> {
    const TABLES_FILE: &str = "etc/acpi/tables";
    const RSDP_FILE: &str = "etc/acpi/rsdp";

    let list = list_acpi_tables(tables)?;

    let find = |sig: &str| -> Result<(u32, u32, u32)> {
        list.iter()
            .find(|(s, ..)| s.as_slice() == sig.as_bytes())
            .map(|&(_, off, csum, len)| (off, csum, len))
            .ok_or_else(|| anyhow!("Required ACPI table missing: {sig}"))
    };
    let (dsdt_offset, dsdt_csum, dsdt_len) = find("DSDT")?;
    let (facp_offset, facp_csum, facp_len) = find("FACP")?;
    let (rsdt_offset, rsdt_csum, rsdt_len) = find("RSDT")?;

    let mut loader = TableLoader::new();
    loader.append(LoaderCmd::Allocate { file: RSDP_FILE, alignment: 16, zone: 2 });
    loader.append(LoaderCmd::Allocate { file: TABLES_FILE, alignment: 64, zone: 1 });
    loader.append(LoaderCmd::AddChecksum {
        file: TABLES_FILE,
        result_offset: dsdt_csum,
        start: dsdt_offset,
        length: dsdt_len,
    });
    for ptr_offset in [36u32, 40] {
        loader.append(LoaderCmd::AddPtr {
            pointer_file: TABLES_FILE,
            pointee_file: TABLES_FILE,
            pointer_offset: facp_offset + ptr_offset,
            pointer_size: 4,
        });
    }
    loader.append(LoaderCmd::AddPtr {
        pointer_file: TABLES_FILE,
        pointee_file: TABLES_FILE,
        pointer_offset: facp_offset + 140,
        pointer_size: 8,
    });
    loader.append(LoaderCmd::AddChecksum {
        file: TABLES_FILE,
        result_offset: facp_csum,
        start: facp_offset,
        length: facp_len,
    });
    // Non-DSDT/FACP/RSDT secondary tables in their file order, e.g. APIC, HPET,
    // MCFG, WAET. FACS lives in the same blob but has no Checksum slot and is
    // wired to FACP via the FIRMWARE_CTRL pointer above, so skip it here.
    for (sig, off, csum, len) in &list {
        if matches!(sig.as_slice(), b"FACS" | b"DSDT" | b"FACP" | b"RSDT") {
            continue;
        }
        loader.append(LoaderCmd::AddChecksum {
            file: TABLES_FILE,
            result_offset: *csum,
            start: *off,
            length: *len,
        });
    }
    // RSDT lists every non-DSDT table; emit one 4-byte AddPtr per entry slot.
    const RSDT_HEADER_LEN: u32 = 36;
    if rsdt_len < RSDT_HEADER_LEN || (rsdt_len - RSDT_HEADER_LEN) % 4 != 0 {
        bail!("Malformed RSDT length: {rsdt_len}");
    }
    let rsdt_entries = (rsdt_len - RSDT_HEADER_LEN) / 4;
    for i in 0..rsdt_entries {
        loader.append(LoaderCmd::AddPtr {
            pointer_file: TABLES_FILE,
            pointee_file: TABLES_FILE,
            pointer_offset: rsdt_offset + RSDT_HEADER_LEN + i * 4,
            pointer_size: 4,
        });
    }
    loader.append(LoaderCmd::AddChecksum {
        file: TABLES_FILE,
        result_offset: rsdt_csum,
        start: rsdt_offset,
        length: rsdt_len,
    });
    loader.append(LoaderCmd::AddPtr {
        pointer_file: RSDP_FILE,
        pointee_file: TABLES_FILE,
        pointer_offset: 16,
        pointer_size: 4,
    });
    loader.append(LoaderCmd::AddChecksum {
        file: RSDP_FILE,
        result_offset: 8,
        start: 0,
        length: 20,
    });

    if loader.buffer.len() < LDR_LENGTH {
        loader.buffer.resize(LDR_LENGTH, 0);
    }
    Ok(loader.buffer)
}

/// An enum to represent the different QEMU loader commands in a type-safe way.
#[derive(Debug)]
enum LoaderCmd<'a> {
    Allocate {
        file: &'a str,
        alignment: u32,
        zone: u8,
    },
    AddPtr {
        pointer_file: &'a str,
        pointee_file: &'a str,
        pointer_offset: u32,
        pointer_size: u8,
    },
    AddChecksum {
        file: &'a str,
        result_offset: u32,
        start: u32,
        length: u32,
    },
}

/// Builder for QEMU-specific loader commands that instruct firmware how to load and patch ACPI tables.
struct TableLoader {
    /// Buffer containing serialized QEMU loader commands
    buffer: Vec<u8>,
}

impl TableLoader {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(LDR_LENGTH),
        }
    }

    /// Appends a fixed-length, null-padded string to the data buffer.
    fn append_fixed_string(data: &mut Vec<u8>, s: &str) {
        let mut s_bytes = s.as_bytes().to_vec();
        s_bytes.resize(FIXED_STRING_LEN, 0);
        data.extend_from_slice(&s_bytes);
    }

    fn append(&mut self, cmd: LoaderCmd) {
        match cmd {
            LoaderCmd::Allocate {
                file,
                alignment,
                zone,
            } => {
                self.buffer.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
                Self::append_fixed_string(&mut self.buffer, file);
                self.buffer.extend_from_slice(&alignment.to_le_bytes());
                self.buffer.push(zone);
                self.buffer.resize(self.buffer.len() + 63, 0); // Padding
            }
            LoaderCmd::AddPtr {
                pointer_file,
                pointee_file,
                pointer_offset,
                pointer_size,
            } => {
                self.buffer.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);
                Self::append_fixed_string(&mut self.buffer, pointer_file);
                Self::append_fixed_string(&mut self.buffer, pointee_file);
                self.buffer.extend_from_slice(&pointer_offset.to_le_bytes());
                self.buffer.push(pointer_size);
                self.buffer.resize(self.buffer.len() + 7, 0); // Padding
            }
            LoaderCmd::AddChecksum {
                file,
                result_offset,
                start,
                length,
            } => {
                self.buffer.extend_from_slice(&[0x03, 0x00, 0x00, 0x00]);
                Self::append_fixed_string(&mut self.buffer, file);
                self.buffer.extend_from_slice(&result_offset.to_le_bytes());
                self.buffer.extend_from_slice(&start.to_le_bytes());
                self.buffer.extend_from_slice(&length.to_le_bytes());
                self.buffer.resize(self.buffer.len() + 56, 0); // Padding
            }
        }
    }
}

/// Searches for an ACPI table with the given signature and returns its offset,
/// checksum offset, and length.
fn find_acpi_table(tables: &[u8], signature: &str) -> Result<(u32, u32, u32)> {
    if signature.len() != 4 {
        bail!("Signature must be 4 characters long, but got '{signature}'");
    }

    let sig_bytes = signature.as_bytes();

    let mut offset = 0;
    while offset < tables.len() {
        // Ensure there's enough space for a table header
        if offset + 8 > tables.len() {
            bail!("Table not found: {signature}");
        }

        let tbl_sig = &tables[offset..offset + 4];
        let tbl_len_bytes: [u8; 4] = tables[offset + 4..offset + 8].try_into().unwrap();
        let tbl_len = u32::from_le_bytes(tbl_len_bytes) as usize;

        if tbl_sig == sig_bytes {
            // Found the table
            return Ok((offset as u32, (offset + 9) as u32, tbl_len as u32));
        }

        if tbl_len == 0 {
            // Invalid table length, stop searching
            bail!("Found table with zero length at offset {offset}");
        }
        // Move to the next table
        offset += tbl_len;
    }

    bail!("Table not found: {signature}");
}

/// Describes how to fetch the QEMU source package for a given distribution.
struct QemuPkg<'a> {
    /// `ppa` -> `pull-ppa-source --ppa ppa:kobuk-team/tdx-release qemu $VERSION`
    /// `main` -> `pull-lp-source qemu $VERSION` (or latest in main when empty)
    source: &'static str,
    /// Version handed to the source fetcher. Empty string == "let the fetcher
    /// pick the current main-archive version" (only meaningful for `main`).
    version: &'a str,
}

fn qemu_pkg_for<'a>(distribution: &str, version_override: Option<&'a str>) -> Result<QemuPkg<'a>> {
    // Pinned defaults for reproducibility; override via `--qemu-version`.
    let (source, default_version): (&'static str, &'static str) = match distribution {
        "ubuntu:25.04" => ("ppa",  "1:9.2.1+ds-1ubuntu4+tdx2.0~ppa2"),
        "ubuntu:26.04" => ("main", "1:10.2.1+ds-1ubuntu4"),
        other => bail!(
            "Unsupported distribution: {other}. Supported: ubuntu:25.04, ubuntu:26.04"
        ),
    };
    Ok(QemuPkg {
        source,
        version: version_override.unwrap_or(default_version),
    })
}

/// Builds the QEMU command line that the patched in-container QEMU will run to
/// dump `etc/acpi/tables`. When `qemu` is `Some(_)`, the command is taken
/// verbatim from the block plus the seven measurement-related core flags;
/// nothing else is added implicitly. When `qemu` is `None`, the script falls
/// back to the Canonical direct-boot defaults so Intel's reference scenario
/// keeps working unchanged.
fn build_qemu_args(qemu: Option<&QemuShape>, cpus: u8, memory: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let push = |args: &mut Vec<OsString>, k: &str, v: &str| {
        args.push(k.into());
        args.push(v.into());
    };

    match qemu {
        Some(q) => {
            push(&mut args, "-accel",     &q.accel);
            push(&mut args, "-m",         memory);
            push(&mut args, "-smp",       &format!("{cpus},maxcpus={cpus}"));
            push(&mut args, "-cpu",       &q.cpu);
            args.push("-no-reboot".into());
            args.push("-nodefaults".into());
            push(&mut args, "-vga",       "none");
            args.push("-nographic".into());
            push(&mut args, "-bios",      OVMF_IN_CONTAINER);
            push(&mut args, "-machine",   &q.machine);
            for v in &q.globals { push(&mut args, "-global", v); }
            for v in &q.objects { push(&mut args, "-object", v); }
            for v in &q.netdevs { push(&mut args, "-netdev", v); }
            for v in &q.devices { push(&mut args, "-device", v); }
            for v in &q.fw_cfg  { push(&mut args, "-fw_cfg", v); }
        }
        None => {
            // Canonical direct-boot defaults: minimal args from
            // https://github.com/canonical/tdx/blob/3.3/guest-tools/direct-boot/boot_direct.sh#L54
            push(&mut args, "-accel",   "kvm");
            push(&mut args, "-m",       memory);
            push(&mut args, "-smp",     &cpus.to_string());
            push(&mut args, "-cpu",     "host");
            push(&mut args, "-machine", "q35,kernel-irqchip=split,hpet=off,smm=off,pic=off");
            push(&mut args, "-bios",    OVMF_IN_CONTAINER);
            args.push("-nographic".into());
            args.push("-nodefaults".into());
            push(&mut args, "-serial",  "stdio");
        }
    }
    args
}

/// Resolves a metadata-relative path against `metadata_path`'s parent.
fn resolve_metadata_path(metadata_path: &Path, path: &str) -> PathBuf {
    metadata_path.parent().unwrap_or(Path::new(".")).join(path)
}

/// Returns the host's `kvm` group id, if the group exists. Used to grant the
/// container access to `/dev/kvm` and `/dev/vhost-vsock`.
fn kvm_group_id() -> Option<String> {
    let out = Command::new("getent").args(["group", "kvm"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .next()
        .and_then(|line| line.split(':').nth(2).map(str::to_owned))
}

fn build_docker_image(
    dockerfile_dir: &Path,
    distribution: &str,
    pkg: &QemuPkg,
    acpi_tables_name: &str,
) -> Result<()> {
    info!("Building Docker image: {IMAGE_NAME}");
    match pkg.source {
        "ppa" => info!(
            "QEMU source: ppa:kobuk-team/tdx-release (Intel TDX-patched QEMU {}) on {distribution}",
            pkg.version
        ),
        "main" => info!(
            "QEMU source: {distribution} main archive ({})",
            if pkg.version.is_empty() { "latest" } else { pkg.version }
        ),
        other => info!(
            "QEMU source: {other} ({})",
            if pkg.version.is_empty() { "?" } else { pkg.version }
        ),
    }

    let status = Command::new("docker")
        .arg("build")
        .args(["--progress", "plain", "--tag", IMAGE_NAME])
        .arg("--build-arg").arg(format!("DISTRIBUTION={distribution}"))
        .arg("--build-arg").arg(format!("QEMU_SOURCE={}", pkg.source))
        .arg("--build-arg").arg(format!("QEMU_VERSION={}", pkg.version))
        .arg("--build-arg").arg(format!("ACPI_TABLES_NAME={acpi_tables_name}"))
        .arg("--file").arg(dockerfile_dir.join("Dockerfile.qemu-acpi-dump"))
        .arg(dockerfile_dir)
        .status()
        .context("Failed to invoke `docker build`")?;
    if !status.success() {
        bail!("docker build failed (exit {status})");
    }
    Ok(())
}

fn run_docker_container(
    bios: &Path,
    output_dir: &Path,
    qemu_args: &[OsString],
    need_kvm: bool,
    need_vhost_vsock: bool,
) -> Result<()> {
    info!("Running QEMU container to generate ACPI tables...");
    // The Dockerfile skips `subdir('pc-bios')` in QEMU's meson build (the option
    // ROMs aren't needed to dump ACPI tables and adding them ~doubles image size).
    // As a result QEMU prints "rom: file kvmvapic.bin … No such file or directory"
    // (and similar for `linuxboot_dma.bin` if a PCI NIC is in play). Harmless —
    // the ACPI dump runs before any ROM would be loaded.
    warn!(
        "NOTE: harmless ROM-file errors (kvmvapic.bin, linuxboot_dma.bin, ...) from QEMU are expected; \
         pc-bios is skipped in the patched build."
    );

    let mut cmd = Command::new("docker");
    cmd.args(["run", "--rm", "--name", CONTAINER_NAME]);

    // The `kvm` group also owns `/dev/vhost-vsock` on Linux, so add it whenever
    // either device matters.
    if let Some(gid) = kvm_group_id() {
        cmd.arg("--group-add").arg(gid);
    }
    if need_kvm {
        if !Path::new("/dev/kvm").exists() {
            bail!(
                "accelerator requires /dev/kvm but the host has none \
                 (use `accel: \"tcg\"` in the qemu block to skip KVM)"
            );
        }
        cmd.args(["--device", "/dev/kvm:/dev/kvm"]);
    }
    if need_vhost_vsock && Path::new("/dev/vhost-vsock").exists() {
        cmd.args(["--device", "/dev/vhost-vsock:/dev/vhost-vsock"]);
    }
    cmd.arg("-v").arg(format!("{}:{OVMF_IN_CONTAINER}:ro", bios.display()));
    cmd.arg("-v").arg(format!("{}:/output", output_dir.display()));
    cmd.arg(IMAGE_NAME);
    cmd.args(qemu_args);

    let status = cmd.status().context("Failed to invoke `docker run`")?;
    if !status.success() {
        bail!("QEMU execution failed (exit {status})");
    }
    Ok(())
}

/// Generates ACPI tables for direct boot by building and running a
/// patched-QEMU Docker container. The patched QEMU writes the
/// `etc/acpi/tables` blob it would have exposed via fw_cfg to
/// `boot_config.acpi_tables` and exits before TD entry.
pub fn generate_acpi_tables(
    metadata_path: &Path,
    distribution: &str,
    qemu_version: Option<&str>,
) -> Result<()> {
    let pkg = qemu_pkg_for(distribution, qemu_version)?;

    let raw_metadata = fs_err::read_to_string(metadata_path)
        .context("Failed to read metadata.json for ACPI generation")?;
    let image_config: ImageConfig = serde_json::from_str(&raw_metadata)
        .context("Failed to parse metadata.json for ACPI generation")?;
    let boot_config = image_config
        .boot_config
        .as_ref()
        .context("boot_config is required to generate ACPI tables")?;
    let bios = resolve_metadata_path(metadata_path, &boot_config.bios)
        .canonicalize()
        .with_context(|| format!("BIOS file not found: {}", boot_config.bios))?;
    let acpi_tables_target = resolve_metadata_path(metadata_path, &boot_config.acpi_tables);
    let acpi_tables_dir = acpi_tables_target
        .parent()
        .with_context(|| format!("acpi_tables has no parent dir: {}", acpi_tables_target.display()))?;
    fs_err::create_dir_all(acpi_tables_dir)?;
    let acpi_tables_name = acpi_tables_target
        .file_name()
        .and_then(OsStr::to_str)
        .context("acpi_tables path must end with a filename")?;
    info!(
        "ACPI gen config: cpus={}, memory={}, bios={}, target={}",
        boot_config.cpus, boot_config.memory, bios.display(), acpi_tables_target.display()
    );
    if let Some(q) = &boot_config.qemu {
        info!(
            "Using boot_config.qemu shape: machine='{}' cpu='{}' accel='{}'",
            q.machine, q.cpu, q.accel
        );
    }

    // Stage the Dockerfile into a temp build context so `docker build` finds it.
    let build_ctx = tempfile::tempdir().context("Failed to create docker build context")?;
    fs_err::write(
        build_ctx.path().join("Dockerfile.qemu-acpi-dump"),
        DOCKERFILE_QEMU_ACPI_DUMP,
    )?;
    build_docker_image(build_ctx.path(), distribution, &pkg, acpi_tables_name)?;

    // Bind-mounted output dir must be writable by the container's non-root `qemu-user`.
    use std::os::unix::fs::PermissionsExt;
    let output_dir = tempfile::tempdir().context("Failed to create ACPI output dir")?;
    fs_err::set_permissions(output_dir.path(), std::fs::Permissions::from_mode(0o777))?;

    let qemu_args = build_qemu_args(boot_config.qemu.as_ref(), boot_config.cpus, &boot_config.memory);
    let need_kvm = boot_config
        .qemu
        .as_ref()
        .map(|q| q.accel == "kvm")
        .unwrap_or(true);
    let need_vhost_vsock = boot_config.qemu.is_some();

    run_docker_container(&bios, output_dir.path(), &qemu_args, need_kvm, need_vhost_vsock)?;

    // Move the produced ACPI tables into place; `fs::copy` would inherit the
    // container's restrictive 0600 from the source, so widen to 0644 after.
    let produced = output_dir.path().join(acpi_tables_name);
    if !produced.exists() {
        bail!("ACPI tables not found in container output: {}", produced.display());
    }
    fs_err::copy(&produced, &acpi_tables_target)?;
    fs_err::set_permissions(&acpi_tables_target, std::fs::Permissions::from_mode(0o644))?;
    info!("ACPI tables written to: {}", acpi_tables_target.display());

    Ok(())
}
