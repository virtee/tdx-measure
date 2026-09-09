//! Walks `tests/fixtures/<name>/` and asserts the tool reproduces the captured
//! `expected.json` measurement for each metadata.json + acpi_tables.bin pair.
//!
//! These are integration tests: they exercise the public library API end-to-end
//! (parse metadata, derive `rsdp`/`table_loader` from the ACPI blob, compute the
//! TD HOB, CFV, and ACPI event-log digests, fold into RTMR0) but stay entirely
//! in-process. No docker, no KVM. They run anywhere `cargo test` runs.
//!
//! New fixtures are picked up automatically by globbing the fixtures dir; see
//! tests/fixtures/README.md for the on-disk layout and tests/regen_fixtures.sh
//! for how the bytes were produced.

use std::path::{Path, PathBuf};

use tdx_measure::{ImageConfig, Machine};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn ovmf_path() -> PathBuf {
    fixtures_dir().join("OVMF.fd")
}

/// Returns each `<fixture>/` under `tests/fixtures/` that ships a
/// `metadata.json`. Bare files (OVMF.fd, README) are skipped.
fn discover_fixtures() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fixtures_dir()
        .read_dir()
        .expect("tests/fixtures missing")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("metadata.json").is_file())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures found under tests/fixtures/");
    out
}

#[derive(Debug, serde::Deserialize)]
struct Expected {
    mrtd: String,
    rtmr0: String,
}

/// Reads metadata.json, resolves the BIOS reference against `ovmf_path()`
/// instead of the path in the json (so the test doesn't need to bake the
/// absolute repo path into the checked-in fixture), and builds a Machine the
/// caller can `measure_platform()` on.
fn machine_for(fixture: &Path) -> (ImageConfig, PathBuf) {
    let raw = std::fs::read_to_string(fixture.join("metadata.json"))
        .expect("read metadata.json");
    let cfg: ImageConfig = serde_json::from_str(&raw).expect("parse metadata.json");
    (cfg, fixture.join("metadata.json"))
}

#[test]
fn ovmf_present() {
    assert!(
        ovmf_path().is_file(),
        "tests/fixtures/OVMF.fd missing — run `tests/fetch_ovmf.sh`"
    );
}

#[test]
fn measure_platform_matches_expected_json_for_every_fixture() {
    for fixture in discover_fixtures() {
        let name = fixture.file_name().unwrap().to_string_lossy().into_owned();
        let expected: Expected = serde_json::from_reader(
            std::fs::File::open(fixture.join("expected.json"))
                .expect("read expected.json"),
        )
        .expect("parse expected.json");

        let (cfg, metadata_path) = machine_for(&fixture);
        let bc = cfg.boot_config.as_ref().expect("boot_config required");

        // Resolve fixture-relative paths the same way the CLI does, but pin
        // the BIOS to the shared OVMF.fd so the fixture is host-independent.
        let parent = metadata_path.parent().unwrap();
        let acpi = parent.join(&bc.acpi_tables).display().to_string();
        let bios = ovmf_path().display().to_string();

        let machine = Machine::builder()
            .cpu_count(bc.cpus)
            .memory_size(cfg.memory_size().expect("memory_size"))
            .firmware(bios.as_str())
            .kernel_cmdline("")
            .acpi_tables(acpi.as_str())
            .rsdp("")
            .table_loader("")
            .boot_order("")
            .path_boot_xxxx("")
            .kernel("/dev/null")
            .initrd("/dev/null")
            .qcow2("")
            .mok_list("")
            .mok_list_trusted("")
            .mok_list_x("")
            .sbat_level("")
            .direct_boot(true)
            .metadata_path(&metadata_path)
            .create_acpi_table(false)
            .distribution("")
            .patch_kernel(true)
            .build();

        let m = machine
            .measure_platform()
            .unwrap_or_else(|e| panic!("measure_platform failed for {name}: {e:#}"));
        assert_eq!(
            hex::encode(&m.mrtd),
            expected.mrtd,
            "mrtd mismatch for {name}"
        );
        assert_eq!(
            hex::encode(&m.rtmr0),
            expected.rtmr0,
            "rtmr0 mismatch for {name}"
        );
    }
}
