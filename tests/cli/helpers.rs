// Copyright (c) 2026 Intel Corporation
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Path to the `tdx-measure` CLI binary.
pub fn cli() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut targets: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        targets.push(PathBuf::from(dir));
    }
    // Default workspace target dir.
    targets.push(manifest.join("target"));
    // Fallback for setups that build from within the cli/ crate.
    targets.push(manifest.join("cli").join("target"));

    for target in targets {
        for profile in ["debug", "release"] {
            let candidate = target.join(profile).join("tdx-measure");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    PathBuf::from("tdx-measure")
}

/// A direct-boot fixture that ships a captured `acpi_tables.bin`, so a plain `--platform-only` measurement succeeds without generating tables.
pub fn direct_boot_metadata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("q35-with-hpet")
        .join("metadata.json")
}

/// Run the CLI with `--platform-only --direct-boot true` plus additional args.
pub fn run(args: &[&str]) -> Output {
    Command::new(cli())
        .arg(direct_boot_metadata())
        .args(["--platform-only", "--direct-boot", "true"])
        .args(args)
        .output()
        .expect("spawn tdx-measure")
}
