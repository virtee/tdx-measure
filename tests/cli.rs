// Copyright (c) 2026 Intel Corporation
// SPDX-License-Identifier: Apache-2.0

//! Integration-test entry point for CLI-level tests.

#[path = "cli/helpers.rs"]
mod helpers;

#[path = "cli/distribution_flag.rs"]
mod distribution_flag;

#[path = "cli/ovmf_version_flag.rs"]
mod ovmf_version_flag;

#[path = "cli/qemu_flags.rs"]
mod qemu_flags;
