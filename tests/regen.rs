//! Slow integration tests that drive `--create-acpi-tables` end-to-end through
//! Docker and assert the resulting bytes are identical to each fixture's
//! checked-in `acpi_tables.bin`. Gated behind `#[ignore]` because they need:
//!
//!   - Docker (with buildx)
//!   - Internet (to fetch the Ubuntu QEMU source on first build of the image)
//!   - A few minutes to compile QEMU
//!
//! For the `canonical-defaults` fixture (no `qemu` block in metadata.json) the
//! tool falls through to the Canonical direct-boot args, which hardcode
//! `-accel kvm -cpu host`. That path additionally needs KVM on the host, so it
//! is skipped on KVM-less runners. The other fixtures pin `accel: "tcg"` in
//! their metadata.json and therefore run anywhere x86_64 Docker images run.
//!
//! Run locally with:
//!   $ cargo test --release --test regen -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn cli_binary() -> PathBuf {
    // cargo's tests put the per-package target dir at
    // $CARGO_MANIFEST_DIR/cli/target/release/tdx-measure when the cli subcrate
    // is built; fall back to PATH lookup if not pre-built.
    let candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cli")
        .join("target")
        .join("release")
        .join("tdx-measure");
    if candidate.is_file() {
        candidate
    } else {
        PathBuf::from("tdx-measure")
    }
}

fn fixtures_to_regen() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fixtures_dir()
        .read_dir()
        .expect("tests/fixtures missing")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("metadata.json").is_file())
        .collect();
    out.sort();
    out
}

/// Reads `boot_config.qemu.accel` from a fixture's metadata.json, defaulting to
/// `"kvm"` if the block is absent (the Canonical-fallback path).
fn fixture_accel(fixture: &Path) -> String {
    let raw = std::fs::read_to_string(fixture.join("metadata.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    json["boot_config"]["qemu"]["accel"]
        .as_str()
        .unwrap_or("kvm")
        .to_string()
}

fn host_has_kvm() -> bool {
    Path::new("/dev/kvm").exists()
}

#[test]
#[ignore = "needs Docker + buildx + several minutes; run with --ignored"]
fn create_acpi_tables_reproduces_each_fixture_byte_for_byte() {
    let cli = cli_binary();
    assert!(
        cli.exists() || which("tdx-measure"),
        "build the CLI first: (cd cli && cargo build --release)",
    );

    for fixture in fixtures_to_regen() {
        let name = fixture.file_name().unwrap().to_string_lossy().into_owned();
        let accel = fixture_accel(&fixture);
        if accel == "kvm" && !host_has_kvm() {
            eprintln!("skipping {name}: needs /dev/kvm (accel=kvm in metadata)");
            continue;
        }

        let captured = fixture.join("acpi_tables.bin");
        let expected_bytes = std::fs::read(&captured).expect("read captured acpi_tables.bin");

        // Move the captured file aside so the tool can write a fresh one in
        // its place; the comparison happens after.
        let stash = fixture.join("acpi_tables.bin.stash");
        std::fs::rename(&captured, &stash).expect("stash captured bytes");

        // Each fixture has been tested against ubuntu:25.04 (Canonical fallback)
        // or ubuntu:26.04 (qemu block). Pick by inspecting the metadata.
        let distro = if accel == "kvm" { "ubuntu:25.04" } else { "ubuntu:26.04" };

        let status = Command::new(&cli)
            .arg(fixture.join("metadata.json"))
            .args(["--platform-only", "--direct-boot", "true"])
            .args(["--create-acpi-tables", distro])
            .status();

        // Always restore the captured copy, even on error.
        if !captured.exists() {
            // Re-stash so the post-comparison can fail meaningfully.
            std::fs::rename(&stash, &captured).expect("restore captured bytes");
            panic!("{name}: --create-acpi-tables didn't produce acpi_tables.bin (status={status:?})");
        }
        let regenerated = std::fs::read(&captured).expect("read regenerated acpi_tables.bin");
        // Replace the just-generated file with the captured one so subsequent
        // test runs see a stable working tree.
        std::fs::rename(&stash, &captured).expect("restore captured bytes");

        assert_eq!(
            regenerated.len(),
            expected_bytes.len(),
            "{name}: regenerated len differs (got {}, expected {})",
            regenerated.len(),
            expected_bytes.len(),
        );
        assert!(
            regenerated == expected_bytes,
            "{name}: regenerated acpi_tables.bin differs from checked-in fixture",
        );
    }
}

fn which(prog: &str) -> bool {
    Command::new("which")
        .arg(prog)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
