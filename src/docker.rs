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

pub(crate) const DOCKERFILE_QEMU: &str = include_str!("../Dockerfile.qemu");
const CONTAINER_NAME: &str = "acpi-tables-generator";
const ACPI_TABLES_IMAGE_NAME: &str = "acpi-tables-generator";
const VERSION_RESOLVER_IMAGE_NAME: &str = "qemu-version-resolver";
pub(crate) const OVMF_IN_CONTAINER: &str = "/usr/share/ovmf/OVMF.fd";

/// Describes how to fetch the QEMU source package for a given distribution.
#[derive(Debug)]
pub struct QemuPkg<'a> {
    /// `ppa:<OWNER>/<NAME>` -> `pull-ppa-source --ppa ppa:<OWNER>/<NAME> qemu $VERSION`
    /// `main` -> `pull-lp-source qemu $VERSION`
    pub source: &'a str,
    /// Version handed to the source fetcher. Empty string == "fetch the latest version for the base image's release", for both `ppa:` and `main` sources.
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

/// Determines the QEMU_SOURCE arg for the Dockerfile.
/// `"ppa:<owner>/<name>"` -> Dockerfile uses `"ppa"` and passes the PPA name via the QEMU_PPA build-arg.
/// `"main"` -> Dockerfile uses `"main"`.
fn resolve_docker_source(source: &str) -> (String, Option<String>) {
    if let Some(ppa) = source.strip_prefix("ppa:") {
        ("ppa".to_string(), Some(format!("ppa:{ppa}")))
    } else {
        (source.to_string(), None)
    }
}

/// Returns a value unique to this invocation, used for transient image tags and cache-busting build args.
fn unique_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

/// Builds the `docker build` command shared by the fetcher and full-build images.
/// `qemu_version` is passed explicitly so a caller can pin a version resolved by an earlier fetcher run.
/// An empty `qemu_version` means "latest in the archive", so a unique `QEMU_FETCH_NONCE` is passed to stop Docker from reusing a cached fetch of an older tip.
fn docker_build_cmd(
    dockerfile_dir: &Path,
    pkg: &QemuPkg,
    qemu_version: &str,
    tag: &str,
    target: Option<&str>,
) -> Command {
    let (qemu_source, qemu_ppa_arg) = resolve_docker_source(pkg.source);

    let mut cmd = Command::new("docker");
    cmd.arg("build").args(["--progress", "plain", "--tag", tag]);
    if let Some(target) = target {
        cmd.args(["--target", target]);
    }
    cmd.arg("--build-arg")
        .arg(format!("DISTRIBUTION={}", pkg.base_image))
        .arg("--build-arg")
        .arg(format!("QEMU_SOURCE={qemu_source}"))
        .arg("--build-arg")
        .arg(format!("QEMU_VERSION={qemu_version}"));
    if let Some(qemu_ppa) = &qemu_ppa_arg {
        cmd.arg("--build-arg").arg(format!("QEMU_PPA={qemu_ppa}"));
    }
    if qemu_version.is_empty() {
        cmd.arg("--build-arg")
            .arg(format!("QEMU_FETCH_NONCE={}", unique_suffix()));
    }
    cmd.arg("--file")
        .arg(dockerfile_dir.join("Dockerfile.qemu"))
        .arg(dockerfile_dir);
    cmd
}

pub(crate) fn build_docker_image(
    dockerfile_dir: &Path,
    pkg: &QemuPkg,
    acpi_tables_name: &str,
) -> Result<()> {
    info!("Building Docker image: {ACPI_TABLES_IMAGE_NAME}");
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

    let mut cmd = docker_build_cmd(
        dockerfile_dir,
        pkg,
        pkg.version,
        ACPI_TABLES_IMAGE_NAME,
        None,
    );
    cmd.arg("--build-arg")
        .arg(format!("ACPI_TABLES_NAME={acpi_tables_name}"));

    let status = cmd.status().context("Failed to invoke `docker build`")?;
    if !status.success() {
        bail!("docker build failed (exit {status})");
    }
    Ok(())
}

/// Builds only the fetcher stage, used to learn the concrete QEMU version without a full build.
fn build_version_resolver_image(dockerfile_dir: &Path, pkg: &QemuPkg, tag: &str) -> Result<()> {
    info!("Building Docker version-resolver image: {tag}");
    let mut cmd = docker_build_cmd(dockerfile_dir, pkg, pkg.version, tag, Some("fetcher"));
    let status = cmd
        .status()
        .context("Failed to invoke `docker build` for the version resolver")?;
    if !status.success() {
        bail!("docker build (version resolver) failed (exit {status})");
    }
    Ok(())
}

/// Best-effort removal of a docker image by tag; logs a warning on failure.
fn remove_docker_image(tag: &str) {
    match Command::new("docker").args(["rmi", "-f", tag]).status() {
        Ok(s) if s.success() => {}
        Ok(s) => warn!("docker rmi {tag} failed (exit {s})"),
        Err(e) => warn!("Failed to invoke `docker rmi` for {tag}: {e}"),
    }
}

/// Builds the fetcher stage and runs it to learn the concrete QEMU version the Dockerfile resolves to.
/// The transient version-resolver image is always removed afterwards, even if the run fails.
fn run_version_resolver(dockerfile_dir: &Path, pkg: &QemuPkg) -> Result<String> {
    let tag = format!("{VERSION_RESOLVER_IMAGE_NAME}-{}", unique_suffix());

    build_version_resolver_image(dockerfile_dir, pkg, &tag)?;

    let run_result = Command::new("docker")
        .args(["run", "--rm", tag.as_str()])
        .output()
        .context("Failed to invoke `docker run` for version resolution");

    remove_docker_image(&tag);

    let out = run_result?;
    if !out.status.success() {
        bail!("version-resolver container failed (exit {})", out.status);
    }
    let version = String::from_utf8(out.stdout)
        .context("version-resolver container output is not valid UTF-8")?
        .trim()
        .to_owned();
    if version.is_empty() {
        bail!("version-resolver container returned an empty version string");
    }
    Ok(version)
}

/// Runs only the lightweight fetcher stage to learn which QEMU version the archive currently carries, without performing a full QEMU build or ACPI dump.
/// Used for `--qemu-version ""`.
pub fn resolve_qemu_archive_version(pkg: &QemuPkg) -> Result<String> {
    let build_ctx = tempfile::tempdir().context("Failed to create docker build context")?;
    fs_err::write(build_ctx.path().join("Dockerfile.qemu"), DOCKERFILE_QEMU)?;
    run_version_resolver(build_ctx.path(), pkg)
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
    cmd.arg(ACPI_TABLES_IMAGE_NAME);
    cmd.args(qemu_args);

    let status = cmd.status().context("Failed to invoke `docker run`")?;
    if !status.success() {
        bail!("QEMU execution failed (exit {status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: QemuPkg<'static> = QemuPkg {
        source: "main",
        version: "",
        base_image: "ubuntu:26.04@sha256:abc",
    };

    fn build_args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn fetch_nonce(args: &[String]) -> Option<&str> {
        args.iter()
            .find_map(|a| a.strip_prefix("QEMU_FETCH_NONCE="))
    }

    #[test]
    fn docker_build_cmd_busts_fetch_cache_for_archive_tip() {
        let dir = Path::new("/ctx");
        let first = build_args(&docker_build_cmd(dir, &PKG, "", "t", Some("fetcher")));
        std::thread::sleep(std::time::Duration::from_millis(1));
        let second = build_args(&docker_build_cmd(dir, &PKG, "", "t", Some("fetcher")));

        let first_nonce = fetch_nonce(&first).expect("nonce missing for archive tip");
        let second_nonce = fetch_nonce(&second).expect("nonce missing for archive tip");
        assert!(!first_nonce.is_empty());
        assert_ne!(first_nonce, second_nonce);
    }

    #[test]
    fn docker_build_cmd_keeps_pinned_version_cacheable() {
        let args = build_args(&docker_build_cmd(
            Path::new("/ctx"),
            &PKG,
            "1:10.2.1+ds-1ubuntu4",
            "t",
            None,
        ));
        assert!(args.contains(&"QEMU_VERSION=1:10.2.1+ds-1ubuntu4".to_string()));
        assert_eq!(fetch_nonce(&args), None);
    }
}
