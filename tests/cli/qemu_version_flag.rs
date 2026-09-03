// Copyright (c) 2026 Intel Corporation
// SPDX-License-Identifier: Apache-2.0

//! CLI-level tests for `--qemu-version` validation.
//!
//! These focus on argument validation and do not require KVM.
//! Some invocations may attempt to spawn Docker (e.g. archive-tip resolution for `--qemu-version ""`), but assertions only cover the validation/error messages.

use super::helpers::{run, run_full};

/// `--qemu-version ""` without `--distribution` is rejected.
#[test]
fn empty_qemu_version_without_distribution_is_rejected() {
    let out = run_full(&["--qemu-version", ""]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("--distribution is required when --qemu-version is empty"),
        "stderr missing distribution-required message: {stderr}",
    );
}

/// `--platform-only` does not require `--qemu-version` or `--distribution`.
/// It may fail for unrelated reasons (missing OVMF fixture) but NOT on version validation.
#[test]
fn platform_only_does_not_require_qemu_version() {
    let out = run(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("--distribution or --qemu-version is required"),
        "--platform-only should not demand QEMU version; stderr: {stderr}",
    );
    assert!(
        !stderr.contains("--distribution is required when --qemu-version is empty"),
        "--platform-only should not trigger archive-tip validation; stderr: {stderr}",
    );
}

/// Full direct-boot measurement without `--qemu-version` and without `--distribution` is rejected.
#[test]
fn full_measurement_without_qemu_version_or_distribution_is_rejected() {
    let out = run_full(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("--distribution or --qemu-version is required for direct boot"),
        "stderr missing requirement message: {stderr}",
    );
}

/// `--qemu-version ""` with `--platform-only` (no `--create-acpi-tables`) skips version resolution entirely.
#[test]
fn platform_only_with_explicit_empty_qemu_version_skips_validation() {
    let out = run(&["--qemu-version", ""]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("--distribution is required when --qemu-version is empty"),
        "--platform-only should skip archive-tip validation; stderr: {stderr}",
    );
}

/// `--qemu-version ""` with `--distribution` does not trigger the archive-tip validation error, since `--distribution` alone already satisfies it.
#[test]
fn empty_qemu_version_with_distribution_is_accepted() {
    let out = run_full(&["--qemu-version", "", "--distribution", "ubuntu:26.04"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("--distribution is required when --qemu-version is empty"),
        "--distribution alone should satisfy the archive-tip requirement; stderr: {stderr}",
    );
}

/// The error for `--qemu-version ""` without `--distribution` mentions `--qemu-source`/`--qemu-base-image`.
#[test]
fn empty_qemu_version_error_mentions_source_and_base_image() {
    let out = run_full(&["--qemu-version", ""]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("--qemu-source") && stderr.contains("--qemu-base-image"),
        "error should mention --qemu-source and --qemu-base-image alternatives; stderr: {stderr}",
    );
}

/// An explicit non-empty `--qemu-version` does not require `--distribution`.
#[test]
fn explicit_qemu_version_does_not_require_distribution() {
    let out = run_full(&["--qemu-version", "1:10.2.1+ds-1ubuntu4"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("--distribution is required"),
        "explicit version should not demand --distribution: {stderr}",
    );
    assert!(
        !stderr.contains("--distribution or --qemu-version is required"),
        "explicit version should satisfy the requirement: {stderr}",
    );
}
