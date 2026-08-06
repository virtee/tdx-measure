// Copyright (c) 2026 Intel Corporation
// SPDX-License-Identifier: Apache-2.0

//! CLI-level tests for `--qemu-source` and `--qemu-base-image` Clap constraints.
//!
//! These flags declare `requires = "create_acpi_tables"` in the Clap definition, so passing them without `--create-acpi-tables` should be rejected by Clap before any application logic runs.
//! No Docker or KVM required.

use super::helpers::run;

/// `--qemu-source` without `--create-acpi-tables` is rejected by Clap.
#[test]
fn qemu_source_without_create_acpi_tables_is_rejected() {
    let out = run(&["--qemu-source", "main"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("create-acpi-tables"),
        "stderr should mention the required flag: {stderr}",
    );
}

/// `--qemu-base-image` without `--create-acpi-tables` is rejected by Clap.
#[test]
fn qemu_base_image_without_create_acpi_tables_is_rejected() {
    let out = run(&["--qemu-base-image", "ubuntu:26.04@sha256:abc123"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("create-acpi-tables"),
        "stderr should mention the required flag: {stderr}",
    );
}

/// Both QEMU override flags together with `--create-acpi-tables` pass validation.
/// (The command will still fail because Docker isn't available, but the error should NOT be an argument validation error.)
#[test]
fn qemu_overrides_with_create_acpi_tables_pass_clap_validation() {
    let out = run(&[
        "--qemu-source",
        "main",
        "--qemu-base-image",
        "ubuntu:26.04@sha256:abc123",
        "--create-acpi-tables",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Should not be a validation error
    assert!(
        !stderr.contains("requires --distribution"),
        "should satisfy OR condition without --distribution: {stderr}",
    );
}

/// `--qemu-source` with `--create-acpi-tables` but without `--qemu-base-image` is rejected by Clap.
#[test]
fn qemu_source_without_qemu_base_image_is_rejected() {
    let out = run(&["--qemu-source", "main", "--create-acpi-tables"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("qemu-base-image"),
        "stderr should mention the required co-flag: {stderr}",
    );
}

/// `--qemu-base-image` with `--create-acpi-tables` but without `--qemu-source` is rejected by Clap.
#[test]
fn qemu_base_image_without_qemu_source_is_rejected() {
    let out = run(&[
        "--qemu-base-image",
        "ubuntu:26.04@sha256:abc123",
        "--create-acpi-tables",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure, got success");
    assert!(
        stderr.contains("qemu-source"),
        "stderr should mention the required co-flag: {stderr}",
    );
}
