// Copyright (c) 2026 Intel Corporation
// SPDX-License-Identifier: Apache-2.0

//! CLI-level tests for the `--distribution` flag.
//!
//! These spawn the `tdx-measure` CLI binary and assert on early validation behavior.
//! No Docker or KVM required.
//! The end-to-end ACPI regeneration path lives in the root crate's `tests/regen.rs`.

use super::helpers::run;

/// The unsupported-distribution check fires before ACPI generation even when `--create-acpi-tables` is requested.
/// A bad distro fails fast rather than trying to spin up Docker.
#[test]
fn unsupported_distribution_is_rejected_with_create_acpi_tables() {
    let out = run(&["--distribution", "fedora:41", "--create-acpi-tables"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("Unsupported distribution") && stderr.contains("fedora:41"),
        "stderr missing rejection message: {stderr}",
    );
}

/// An unsupported `--distribution` is rejected even without `--create-acpi-tables`.
#[test]
fn unsupported_distribution_without_create_acpi_tables_is_rejected() {
    let out = run(&["--distribution", "debian:12"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("Unsupported distribution") && stderr.contains("debian:12"),
        "stderr missing rejection message: {stderr}",
    );
}

/// A supported `--distribution` is accepted on its own, decoupled from `--create-acpi-tables`.
/// The helper baseline invocation uses fixture metadata with captured ACPI tables, so no ACPI regeneration is needed.
#[test]
fn supported_distribution_standalone_is_accepted() {
    let out = run(&["--distribution", "ubuntu:26.04"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "supported distribution should be accepted; stderr: {stderr}",
    );
    assert!(
        !stderr.contains("Unsupported distribution"),
        "no rejection expected for a supported distro: {stderr}",
    );
}

/// `--distribution` is optional for the helper baseline measurement path.
/// Omitting it should still succeed and should not trigger distribution validation errors.
#[test]
fn distribution_is_optional_for_baseline_measurement() {
    let out = run(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "baseline measurement without --distribution should succeed; stderr: {stderr}",
    );
    assert!(
        !stderr.contains("Unsupported distribution"),
        "no distribution error expected when the flag is omitted: {stderr}",
    );
}

/// `--create-acpi-tables` without `--distribution` and without explicit `--qemu-source`/`--qemu-base-image` is rejected.
#[test]
fn create_acpi_tables_without_distribution_is_rejected() {
    let out = run(&["--create-acpi-tables"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("--create-acpi-tables requires --distribution"),
        "stderr missing requirement message: {stderr}",
    );
}
