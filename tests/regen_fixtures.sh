#!/usr/bin/env bash
# Regenerate `acpi_tables.bin` + `expected.json` for every fixture under
# tests/fixtures/. Run this when:
#   - the QEMU pin in src/acpi.rs is bumped, or
#   - a fixture's metadata.json is edited.
#
# Bytes are checked into the repo so `cargo test --release --test parse` doesn't
# need Docker or KVM on the developer's machine.
#
# Requirements: Docker (with buildx), and `tests/fixtures/OVMF.fd` already
# fetched (run tests/fetch_ovmf.sh first).
#
# For the `canonical-defaults` fixture (no `qemu` block — falls through to
# Canonical direct-boot args, which hardcode `-accel kvm`) this additionally
# needs KVM on the host. The other fixtures use `accel: "tcg"` and work
# anywhere x86_64 Docker images run.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
fixtures="$here/fixtures"
tdx_measure="$repo/cli/target/release/tdx-measure"

if [[ ! -x "$tdx_measure" ]]; then
    echo "build the CLI first: (cd cli && cargo build --release)" >&2
    exit 1
fi
if [[ ! -f "$fixtures/OVMF.fd" ]]; then
    echo "$fixtures/OVMF.fd missing; run $here/fetch_ovmf.sh first" >&2
    exit 1
fi

# distro chosen per fixture: `canonical-defaults` uses ubuntu:25.04 (matches the
# kobuk-team/tdx-release PPA QEMU 9.2.1 that Canonical's direct-boot reference
# is built against); everything else uses ubuntu:26.04 (main-archive QEMU 10.2.1).
distro_for() {
    case "$1" in
        canonical-defaults) echo "ubuntu:25.04" ;;
        *)                  echo "ubuntu:26.04" ;;
    esac
}

declare -i ok=0 fail=0
for dir in "$fixtures"/*/; do
    name="$(basename "$dir")"
    [[ -f "$dir/metadata.json" ]] || continue

    # A fixture whose `acpi_tables.bin` came from a live TD (via
    # `extract_config_files.py`) ships a `SOURCE-OF-TRUTH.md`. Those bytes are
    # the authoritative reference for the dumper round-trip, so we never
    # overwrite them — just refresh `expected.json`.
    if [[ -f "$dir/SOURCE-OF-TRUTH.md" ]]; then
        echo "=== $name (runtime-captured; preserving acpi_tables.bin) ==="
        rm -f "$dir/expected.json"
        "$tdx_measure" "$dir/metadata.json" --platform-only --json 2>/dev/null \
            > "$dir/expected.json"
        ok+=1
        continue
    fi

    echo "=== $name ==="
    distro="$(distro_for "$name")"
    rm -f "$dir/acpi_tables.bin" "$dir/expected.json"

    if ! "$tdx_measure" "$dir/metadata.json" \
            --platform-only --direct-boot true \
            --create-acpi-tables "$distro" >/dev/null; then
        echo "FAIL $name: --create-acpi-tables failed"
        fail+=1
        continue
    fi

    # `--platform-only --json` runs the measurement pipeline and prints the
    # JSON result to stdout (logs go to stderr).
    "$tdx_measure" "$dir/metadata.json" --platform-only --json 2>/dev/null \
        > "$dir/expected.json"
    ok+=1
done
echo "Done: $ok regenerated, $fail failed"
exit $fail
