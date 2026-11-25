// Copyright (c) 2026 Intel Corporation
// SPDX-License-Identifier: Apache-2.0

//! CLI-level tests for the `--ovmf-version` flag.
//!
//! Spawns the `tdx-measure` binary and asserts on validation behavior.
//! No Docker or KVM required.

use super::helpers::run;
use super::helpers::run_full;
use super::helpers::run_runtime_only;

/// An unrecognized OVMF version is rejected with a clear error.
#[test]
fn invalid_ovmf_version_is_rejected() {
    let out = run(&["--ovmf-version", "2025.11-ubuntu7"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("is invalid"),
        "stderr missing rejection message: {stderr}",
    );
}

/// A valid OVMF version for the given distribution is accepted.
#[test]
fn valid_ovmf_version_with_distribution_is_accepted() {
    let out = run(&[
        "--distribution",
        "ubuntu:26.04",
        "--ovmf-version",
        "edk2-stable202502",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "valid OVMF version should be accepted; stderr: {stderr}",
    );
}

/// A valid upstream OVMF release can override a different distribution default.
#[test]
fn upstream_ovmf_version_override_is_accepted() {
    // Ubuntu 26.04 defaults to ubuntu package 2025.02-3ubuntu2.2, so upstream 2025.02 exercises an explicit override.
    let out = run(&[
        "--distribution",
        "ubuntu:26.04",
        "--ovmf-version",
        "edk2-stable202502",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "upstream OVMF override should be accepted; stderr: {stderr}",
    );
}

#[test]
fn stable_tag_ovmf_version_is_accepted() {
    let out = run(&["--ovmf-version", "edk2-stable202502"]);
    assert!(out.status.success(), "stable tag should be accepted");
}

/// Omitting --ovmf-version uses the distribution default without error.
#[test]
fn omitted_ovmf_version_uses_distribution_default() {
    let out = run(&["--distribution", "ubuntu:25.04"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "omitting --ovmf-version with a supported distro should succeed; stderr: {stderr}",
    );
}

/// An invalid explicit OVMF version is rejected even when distribution defaults are valid.
#[test]
fn invalid_ovmf_version_override_is_not_masked_by_distribution_default() {
    let out = run(&[
        "--distribution",
        "ubuntu:26.04",
        "--ovmf-version",
        "2025.11-ubuntu7",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("is invalid"),
        "stderr missing rejection message: {stderr}",
    );
}

/// Running --runtime-only without --ovmf-version or --distribution fails because the OVMF version is required to determine initrd cmdline ordering.
#[test]
fn missing_ovmf_version_in_runtime_mode_is_rejected() {
    let out = run_runtime_only(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected failure without --ovmf-version; stderr: {stderr}",
    );
}

/// Full direct boot measurement (RTMR1 + RTMR2) without --ovmf-version or --distribution fails.
#[test]
fn full_measurement_without_ovmf_version_is_rejected() {
    let out = run_full(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected failure without --ovmf-version for full measurement; stderr: {stderr}",
    );
}

/// Running --platform-only without --ovmf-version or --distribution is rejected.
/// RTMR0 boot entries differ by OVMF version, so the version is always required for direct boot.
#[test]
fn platform_only_without_ovmf_version_is_rejected() {
    let out: std::process::Output = run(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected failure without --ovmf-version for platform-only direct boot; stderr: {stderr}",
    );
    assert!(
        stderr.contains("OVMF version is required"),
        "stderr missing OVMF requirement message: {stderr}",
    );
}
