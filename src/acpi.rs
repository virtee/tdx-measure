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

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an 8-byte ACPI table header with the given signature and length.
    /// Body bytes (if any) are caller-appended.
    fn header(sig: &[u8; 4], len: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(sig);
        h.extend_from_slice(&len.to_le_bytes());
        h
    }

    /// Concatenates a list of (sig, padded_body_len) into a blob shaped like
    /// `etc/acpi/tables`. Each entry becomes `<8-byte header><(len-8) zeros>`.
    fn build_blob(tables: &[(&[u8; 4], u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for &(sig, len) in tables {
            out.extend(header(sig, len));
            out.resize(out.len() + (len as usize - 8), 0);
        }
        out
    }

    #[test]
    fn find_acpi_table_returns_offsets_and_csum_for_each_table() {
        // Minimal Canonical-defaults table set, no HPET; 4-entry RSDT (len=36+16).
        let blob = build_blob(&[
            (b"FACS", 64),
            (b"DSDT", 200),
            (b"FACP", 244),
            (b"APIC", 144),
            (b"MCFG", 60),
            (b"WAET", 40),
            (b"RSDT", 52),
        ]);

        // Spot-check three tables. csum is the offset of the table's
        // 9th byte (header layout: 4-byte sig, 4-byte length, 1-byte revision,
        // 1-byte checksum -> checksum lives at offset+9).
        let (off, csum, len) = find_acpi_table(&blob, "DSDT").unwrap();
        assert_eq!(off, 64);
        assert_eq!(csum, 73);
        assert_eq!(len, 200);

        let (off, _, len) = find_acpi_table(&blob, "FACP").unwrap();
        assert_eq!(off, 264);
        assert_eq!(len, 244);

        let (off, _, _) = find_acpi_table(&blob, "RSDT").unwrap();
        assert_eq!(off, 64 + 200 + 244 + 144 + 60 + 40); // = 752
    }

    #[test]
    fn list_acpi_tables_walks_blob_and_stops_at_padding() {
        let blob = build_blob(&[
            (b"FACS", 64),
            (b"DSDT", 100),
            (b"FACP", 244),
            (b"RSDT", 40),
        ]);
        let list = list_acpi_tables(&blob).unwrap();
        let sigs: Vec<&[u8]> = list.iter().map(|(s, ..)| s.as_slice()).collect();
        assert_eq!(sigs, [b"FACS".as_slice(), b"DSDT", b"FACP", b"RSDT"]);

        // Padding region (zeros) after the last table must terminate the walk,
        // not be treated as a malformed table.
        let mut blob_padded = blob.clone();
        blob_padded.resize(blob.len() + 4096, 0);
        let list2 = list_acpi_tables(&blob_padded).unwrap();
        assert_eq!(list2.len(), 4);
    }

    /// Decodes one 128-byte QEMU loader command, returning a tuple shaped like
    /// (cmd_type, file_or_ptr_file, optional_pointee_file, payload_u32s).
    /// Used to assert the byte layout of `derive_table_loader` without re-
    /// implementing the encoder.
    fn decode_loader_cmd(cmd: &[u8]) -> (u32, String, Option<String>, Vec<u32>) {
        assert_eq!(cmd.len(), 128);
        let kind = u32::from_le_bytes(cmd[0..4].try_into().unwrap());
        let read_str = |off: usize| -> String {
            let s = &cmd[off..off + FIXED_STRING_LEN];
            let end = s.iter().position(|b| *b == 0).unwrap_or(s.len());
            std::str::from_utf8(&s[..end]).unwrap().to_string()
        };
        match kind {
            // Allocate: file + alignment + zone
            1 => {
                let file = read_str(4);
                let align = u32::from_le_bytes(cmd[60..64].try_into().unwrap());
                let zone = cmd[64] as u32;
                (1, file, None, vec![align, zone])
            }
            // AddPtr: pointer_file + pointee_file + offset + size
            2 => {
                let ptr_file = read_str(4);
                let ptee_file = read_str(60);
                let ptr_off = u32::from_le_bytes(cmd[116..120].try_into().unwrap());
                let ptr_size = cmd[120] as u32;
                (2, ptr_file, Some(ptee_file), vec![ptr_off, ptr_size])
            }
            // AddChecksum: file + result_offset + start + length
            3 => {
                let file = read_str(4);
                let result = u32::from_le_bytes(cmd[60..64].try_into().unwrap());
                let start = u32::from_le_bytes(cmd[64..68].try_into().unwrap());
                let length = u32::from_le_bytes(cmd[68..72].try_into().unwrap());
                (3, file, None, vec![result, start, length])
            }
            other => panic!("unknown loader cmd type {other}"),
        }
    }

    fn split_cmds(loader: &[u8]) -> Vec<&[u8]> {
        assert!(loader.len() >= LDR_LENGTH);
        loader
            .chunks_exact(128)
            .take_while(|chunk| u32::from_le_bytes(chunk[0..4].try_into().unwrap()) != 0)
            .collect()
    }

    #[test]
    fn derive_table_loader_emits_canonical_no_hpet_layout() {
        let blob = build_blob(&[
            (b"FACS", 64),
            (b"DSDT", 100),
            (b"FACP", 244),
            (b"APIC", 144),
            (b"MCFG", 60),
            (b"WAET", 40),
            (b"RSDT", 52), // header (36) + 4 × 4-byte entries
        ]);
        let loader = derive_table_loader(&blob).unwrap();
        let cmds: Vec<_> = split_cmds(&loader).into_iter().map(decode_loader_cmd).collect();

        // Expected command sequence per the docstring on derive_table_loader.
        // Allocate rsdp + Allocate tables + AddChecksum DSDT + 3×AddPtr FACP + AddChecksum FACP
        // + AddChecksum APIC + AddChecksum MCFG + AddChecksum WAET + 4×AddPtr RSDT
        // + AddChecksum RSDT + AddPtr RSDP + AddChecksum RSDP = 17 commands.
        assert_eq!(cmds.len(), 17);
        assert_eq!(cmds[0].0, 1); // Allocate
        assert_eq!(cmds[0].1, "etc/acpi/rsdp");
        assert_eq!(cmds[1].0, 1);
        assert_eq!(cmds[1].1, "etc/acpi/tables");
        assert_eq!(cmds[2].0, 3); // AddChecksum DSDT
        assert_eq!(cmds[2].3, vec![64 + 9, 64, 100]);

        // 3 AddPtr at facp+36, facp+40, facp+140 (offsets relative to the
        // FACP base in the concatenated blob)
        let facp_off = 64 + 100;
        for (i, &expected_off) in [36u32, 40, 140].iter().enumerate() {
            let cmd = &cmds[3 + i];
            assert_eq!(cmd.0, 2);
            assert_eq!(cmd.3[0], facp_off + expected_off);
        }
        // 4 RSDT pointers (no HPET) at rsdt+36/+40/+44/+48
        let rsdt_off = 64 + 100 + 244 + 144 + 60 + 40;
        for (i, expected_off) in (36u32..36 + 4 * 4).step_by(4).enumerate() {
            let cmd = &cmds[10 + i];
            assert_eq!(cmd.0, 2);
            assert_eq!(cmd.3[0], rsdt_off + expected_off);
        }
        // RSDT checksum, then the final RSDP wiring + checksum.
        assert_eq!(cmds[14].0, 3); // AddChecksum RSDT
        assert_eq!(cmds[14].1, "etc/acpi/tables");
        assert_eq!(cmds[15].0, 2); // AddPtr rsdp+16 -> tables
        assert_eq!(cmds[15].1, "etc/acpi/rsdp");
        assert_eq!(cmds[15].3, vec![16, 4]);
        assert_eq!(cmds[16].0, 3); // AddChecksum rsdp
        assert_eq!(cmds[16].1, "etc/acpi/rsdp");
        assert_eq!(cmds[16].3, vec![8, 0, 20]);

        // Buffer must be padded to LDR_LENGTH (4096).
        assert_eq!(loader.len(), LDR_LENGTH);
    }

    #[test]
    fn derive_table_loader_with_hpet_adds_an_extra_addchecksum_and_rsdt_ptr() {
        // Same shape with HPET in the mix. RSDT now has 5 entries (FACP, APIC,
        // HPET, MCFG, WAET) so length = 36 + 5*4 = 56. Regression case for the
        // pre-rewrite code that hardcoded 4 RSDT entries.
        let blob = build_blob(&[
            (b"FACS", 64),
            (b"DSDT", 100),
            (b"FACP", 244),
            (b"APIC", 144),
            (b"HPET", 56),
            (b"MCFG", 60),
            (b"WAET", 40),
            (b"RSDT", 56),
        ]);
        let loader = derive_table_loader(&blob).unwrap();
        let cmds: Vec<_> = split_cmds(&loader).into_iter().map(decode_loader_cmd).collect();

        // 17 + 2 commands now: one extra AddChecksum HPET + one extra AddPtr RSDT entry.
        assert_eq!(cmds.len(), 19);
        // Count AddChecksums in `etc/acpi/tables` (i.e. tables blob, not rsdp).
        let checksum_tables: Vec<_> = cmds.iter()
            .filter(|c| c.0 == 3 && c.1 == "etc/acpi/tables")
            .collect();
        // DSDT + FACP + APIC + HPET + MCFG + WAET + RSDT = 7
        assert_eq!(checksum_tables.len(), 7);
        // Count RSDT-entry AddPtrs (5 vs the 4 the old code emitted).
        let rsdt_off = 64 + 100 + 244 + 144 + 56 + 60 + 40;
        let rsdt_ptrs: Vec<_> = cmds.iter()
            .filter(|c| c.0 == 2 && c.1 == "etc/acpi/tables"
                     && c.3[0] >= rsdt_off + 36 && c.3[0] < rsdt_off + 36 + 5 * 4)
            .collect();
        assert_eq!(rsdt_ptrs.len(), 5);
    }

    #[test]
    fn qemu_pkg_for_known_distros_returns_pinned_defaults() {
        let p = qemu_pkg_for("ubuntu:25.04", None).unwrap();
        assert_eq!(p.source, "ppa");
        assert_eq!(p.version, "1:9.2.1+ds-1ubuntu4+tdx2.0~ppa2");

        let p = qemu_pkg_for("ubuntu:26.04", None).unwrap();
        assert_eq!(p.source, "main");
        assert_eq!(p.version, "1:10.2.1+ds-1ubuntu4");
    }

    #[test]
    fn qemu_pkg_for_honors_version_override() {
        let p = qemu_pkg_for("ubuntu:26.04", Some("1:10.3.0-1ubuntu1")).unwrap();
        assert_eq!(p.source, "main");
        assert_eq!(p.version, "1:10.3.0-1ubuntu1");

        // Even an empty override (caller asked for "latest") wins over the pin.
        let p = qemu_pkg_for("ubuntu:26.04", Some("")).unwrap();
        assert_eq!(p.version, "");
    }

    #[test]
    fn qemu_pkg_for_rejects_unknown_distro() {
        assert!(qemu_pkg_for("debian:12", None).is_err());
        assert!(qemu_pkg_for("", None).is_err());
    }

    #[test]
    fn build_qemu_args_canonical_fallback_matches_dstack_reference() {
        // Canonical direct-boot args from the upstream `dstack`-derived flow.
        // Pinned because the Canonical-defaults scenario depends on this exact order.
        let args = build_qemu_args(None, 4, "2048M");
        let expected: Vec<&str> = vec![
            "-accel", "kvm",
            "-m", "2048M",
            "-smp", "4",
            "-cpu", "host",
            "-machine", "q35,kernel-irqchip=split,hpet=off,smm=off,pic=off",
            "-bios", OVMF_IN_CONTAINER,
            "-nographic",
            "-nodefaults",
            "-serial", "stdio",
        ];
        let got: Vec<&str> = args.iter().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn build_qemu_args_qemu_block_passes_fields_verbatim_and_in_documented_order() {
        let shape = QemuShape {
            machine: "q35,kernel_irqchip=split,smm=off,pic=off".into(),
            cpu: "Skylake-Server,phys-bits=46".into(),
            accel: "tcg".into(),
            globals: vec!["q35-pcihost.pci-hole64-size=4096G".into()],
            objects: vec!["memory-backend-ram,id=mem0,size=16384M".into()],
            netdevs: vec!["hubport,id=net0,hubid=0".into()],
            devices: vec![
                "e1000,netdev=net0,bus=pcie.0,addr=0x2,romfile=".into(),
                "virtio-rng-pci".into(),
            ],
            fw_cfg: vec!["name=opt/ovmf/X-PciMmio64Mb,string=262144".into()],
        };
        let args = build_qemu_args(Some(&shape), 8, "16384M")
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect::<Vec<_>>();

        // Core seven flags first (in a documented order), then -machine, then
        // user-supplied lists in -global / -object / -netdev / -device / -fw_cfg order.
        assert_eq!(args, vec![
            "-accel", "tcg",
            "-m", "16384M",
            "-smp", "8,maxcpus=8",
            "-cpu", "Skylake-Server,phys-bits=46",
            "-no-reboot",
            "-nodefaults",
            "-vga", "none",
            "-nographic",
            "-bios", OVMF_IN_CONTAINER,
            "-machine", "q35,kernel_irqchip=split,smm=off,pic=off",
            "-global", "q35-pcihost.pci-hole64-size=4096G",
            "-object", "memory-backend-ram,id=mem0,size=16384M",
            "-netdev", "hubport,id=net0,hubid=0",
            "-device", "e1000,netdev=net0,bus=pcie.0,addr=0x2,romfile=",
            "-device", "virtio-rng-pci",
            "-fw_cfg", "name=opt/ovmf/X-PciMmio64Mb,string=262144",
        ]);
    }

}
