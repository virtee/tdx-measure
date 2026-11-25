// Copyright (c) 2026 Intel Corporation
// SPDX-License-Identifier: Apache-2.0

//! CLI-level test that `boot_config.smbios` in metadata.json is resolved relative to the metadata file's directory (`PathResolver::new` in cli/src/main.rs), not the process CWD.

use std::path::Path;
use std::process::Command;

use super::helpers::cli;

#[test]
fn relative_smbios_path_is_resolved_against_metadata_dir() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("upstream-edk2-202505");
    let metadata = fixture.join("metadata.json");

    if !fixture.join("OVMF-edk2-stable202505.fd").is_file() {
        eprintln!(
            "skipping: upstream-edk2-202505 fixture firmware missing — run tests/fetch_ovmf.sh"
        );
        return;
    }

    let expected: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(fixture.join("expected.json")).expect("read expected.json"),
    )
    .expect("parse expected.json");

    // Run from a different CWD to prove the path is joined against the metadata file's directory, not the CWD.
    let out = Command::new(cli())
        .current_dir(std::env::temp_dir())
        .arg(&metadata)
        .args([
            "--platform-only",
            "--direct-boot",
            "true",
            "--ovmf-version",
            "edk2-stable202505",
            "--json",
        ])
        .output()
        .expect("spawn tdx-measure");

    assert!(
        out.status.success(),
        "measurement should succeed with smbios resolved relative to metadata.json; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let actual: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse tdx-measure JSON output");
    assert_eq!(
        actual["rtmr0"], expected["rtmr0"],
        "rtmr0 mismatch; smbios path was likely not resolved relative to metadata.json"
    );
}
