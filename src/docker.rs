/*
 * Copyright (c) 2026 Intel Corporation
 * SPDX-License-Identifier: Apache-2.0
 */
//! Generic Docker primitives used to build and run the patched-QEMU container.

use anyhow::{bail, Context, Result};
use log::{info, warn};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

pub(crate) const DOCKERFILE_QEMU_ACPI_DUMP: &str = include_str!("../Dockerfile.qemu-acpi-dump");
const CONTAINER_NAME: &str = "acpi-tables-generator";
const IMAGE_NAME: &str = "acpi-tables-generator";
pub(crate) const OVMF_IN_CONTAINER: &str = "/usr/share/ovmf/OVMF.fd";

/// Describes how to fetch the QEMU source package for a given distribution.
pub struct QemuPkg<'a> {
    /// `ppa:<OWNER>/<NAME>` -> `pull-ppa-source --ppa ppa:<OWNER>/<NAME> qemu $VERSION`
    /// `main` -> `pull-lp-source qemu $VERSION` (or latest in main when empty)
    pub source: &'a str,
    /// Version handed to the source fetcher. Empty string == "let the fetcher
    /// pick the current main-archive version" (only meaningful for `main`).
    pub version: &'a str,
    /// Immutable OCI reference for the base image (e.g. `ubuntu:26.04@sha256:...`).
    pub base_image: &'a str,
}

/// Returns the host's `kvm` group id, if the group exists.
/// Used to grant the container access to `/dev/kvm` and `/dev/vhost-vsock`.
fn kvm_group_id() -> Option<String> {
    let out = Command::new("getent")
        .args(["group", "kvm"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .next()
        .and_then(|line| line.split(':').nth(2).map(str::to_owned))
}

pub(crate) fn build_docker_image(
    dockerfile_dir: &Path,
    pkg: &QemuPkg,
    acpi_tables_name: &str,
) -> Result<()> {
    info!("Building Docker image: {IMAGE_NAME}");
    if pkg.source.starts_with("ppa:") {
        info!("QEMU source: {} (QEMU {})", pkg.source, pkg.version);
    } else {
        info!(
            "QEMU source: {} archive ({})",
            pkg.source,
            if pkg.version.is_empty() {
                "latest"
            } else {
                pkg.version
            }
        );
    }

    // Determine the QEMU_SOURCE arg for the Dockerfile.
    // "ppa:<owner>/<name>" -> Dockerfile uses "ppa" and passes the PPA name via QEMU_PPA build-arg.
    // "main" -> Dockerfile uses "main".
    let (docker_source, ppa_name) = if let Some(ppa) = pkg.source.strip_prefix("ppa:") {
        ("ppa".to_string(), Some(format!("ppa:{ppa}")))
    } else {
        (pkg.source.to_string(), None)
    };

    let mut cmd = Command::new("docker");
    cmd.arg("build")
        .args(["--progress", "plain", "--tag", IMAGE_NAME])
        .arg("--build-arg")
        .arg(format!("DISTRIBUTION={}", pkg.base_image))
        .arg("--build-arg")
        .arg(format!("QEMU_SOURCE={docker_source}"))
        .arg("--build-arg")
        .arg(format!("QEMU_VERSION={}", pkg.version))
        .arg("--build-arg")
        .arg(format!("ACPI_TABLES_NAME={acpi_tables_name}"));
    if let Some(ppa) = &ppa_name {
        cmd.arg("--build-arg").arg(format!("QEMU_PPA={ppa}"));
    }
    cmd.arg("--file")
        .arg(dockerfile_dir.join("Dockerfile.qemu-acpi-dump"))
        .arg(dockerfile_dir);

    let status = cmd.status().context("Failed to invoke `docker build`")?;
    if !status.success() {
        bail!("docker build failed (exit {status})");
    }
    Ok(())
}

pub(crate) fn run_docker_container(
    bios: &Path,
    output_dir: &Path,
    qemu_args: &[OsString],
    need_kvm: bool,
    need_vhost_vsock: bool,
) -> Result<()> {
    info!("Running QEMU container to generate ACPI tables...");
    // The Dockerfile skips `subdir('pc-bios')` in QEMU's meson build (the option ROMs aren't needed to dump ACPI tables and adding them ~doubles image size).
    // As a result QEMU prints "rom: file kvmvapic.bin … No such file or directory" (and similar for `linuxboot_dma.bin` if a PCI NIC is in play).
    // Harmless - the ACPI dump runs before any ROM would be loaded.
    warn!(
        "NOTE: harmless ROM-file errors (kvmvapic.bin, linuxboot_dma.bin, ...) from QEMU are expected; \
         pc-bios is skipped in the patched build."
    );

    let mut cmd = Command::new("docker");
    cmd.args(["run", "--rm", "--name", CONTAINER_NAME]);

    // The `kvm` group also owns `/dev/vhost-vsock` on Linux, so add it whenever either device matters.
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
    cmd.arg("-v")
        .arg(format!("{}:{OVMF_IN_CONTAINER}:ro", bios.display()));
    cmd.arg("-v")
        .arg(format!("{}:/output", output_dir.display()));
    cmd.arg(IMAGE_NAME);
    cmd.args(qemu_args);

    let status = cmd.status().context("Failed to invoke `docker run`")?;
    if !status.success() {
        bail!("QEMU execution failed (exit {status})");
    }
    Ok(())
}
