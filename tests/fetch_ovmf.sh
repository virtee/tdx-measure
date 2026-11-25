#!/usr/bin/env bash
# Ensure the shared and versioned OVMF fixtures for the test suite are present.
#
# Usage: tests/fetch_ovmf.sh [--shared-only|--versioned-only]
#   Fetch shared OVMF.fd (Ubuntu 25.04 edk2 package) to tests/fixtures/OVMF.fd.
#   Build versioned OVMF-edk2-stableYYYYMM.fd images from edk2 tags via ./Dockerfile.edk2-ovmf-build.
#   Versions defined in the versions array below.
#   With no argument, both steps run (the one-time local setup).
#   --shared-only skips the Docker-based versioned build, for CI jobs that must not depend on Docker.
#   --versioned-only skips the shared fetch, for a dedicated CI job that only builds versioned firmware.
#
# Idempotent: skips work only when the target firmware and its validated build marker match.

set -euo pipefail

mode="${1:-all}"
case "$mode" in
    all|--shared-only|--versioned-only) ;;
    *)
        echo "usage: $0 [--shared-only|--versioned-only]" >&2
        exit 1
        ;;
esac

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

# Require jq before fetching or building fixture artifacts.
if ! command -v jq > /dev/null; then
    echo "jq is required; install it before running $0" >&2
    exit 1
fi

# Download the shared Ubuntu OVMF fixture and verify it against its pinned digest.
ensure_shared_ovmf_fixture() {
    local out="$here/fixtures/OVMF.fd"
    local expected_sha256=9e807cb2cd4313406a3aa4becc0836671a5c64ca7bdc08a45e15260184b446bf

    # Reuse the artifact only when its bytes match the pinned package content.
    if [[ -f "$out" ]] \
       && [[ "$(sha256sum "$out" | awk '{print $1}')" == "$expected_sha256" ]]; then
        echo "$out already present and matches expected sha256"
        return 0
    fi

    mkdir -p "$here/fixtures"
    local tmp_deb
    tmp_deb="$(mktemp --suffix=.deb)"
    local extract_dir="${tmp_deb%.deb}.d"
    trap 'rm -rf "$tmp_deb" "$extract_dir"' EXIT

    # Extract only the firmware payload from the pinned Ubuntu OVMF package.
    wget -q https://archive.ubuntu.com/ubuntu/pool/main/e/edk2/ovmf_2025.02-3ubuntu2_all.deb -O "$tmp_deb"
    dpkg-deb -R "$tmp_deb" "$extract_dir"
    cp "$extract_dir/usr/share/ovmf/OVMF.fd" "$out"

    # Reject a changed package payload before it can affect the fixture tests.
    local actual
    actual="$(sha256sum "$out" | awk '{print $1}')"
    if [[ "$actual" != "$expected_sha256" ]]; then
        echo "OVMF.fd sha256 mismatch: got $actual, expected $expected_sha256" >&2
        exit 1
    fi
    echo "$out ready (sha256=$expected_sha256)"
    trap - EXIT
    rm -rf "$tmp_deb" "$extract_dir"
}

# Build one versioned upstream EDK2 OVMF fixture from fully pinned source and toolchain inputs.
ensure_versioned_ovmf_fixture() {
    local edk2_version="$1"
    local edk2_commit="$2"
    local ubuntu_snapshot="$3"
    local debian_patch_version="$4"
    local debian_patch_sha256="$5"
    local expected_firmware_sha256="$6"
    local suffix="${edk2_version#edk2-stable}"

    # Keep fixture directory names stable and reject unsupported release identifiers.
    if [[ "$suffix" == "$edk2_version" || ! "$suffix" =~ ^[0-9]{6}$ ]]; then
        echo "invalid edk2 version '$edk2_version'; expected the form edk2-stableYYYYMM" >&2
        exit 1
    fi

    local out_dir="$here/fixtures/upstream-edk2-${suffix}"
    local out="$out_dir/OVMF-${edk2_version}.fd"
    local version_marker="$out_dir/.edk2-version"
    local build_input_sha256

    # Rebuild when Dockerfile changes alter any part of the reproducible build recipe.
    build_input_sha256="$(sha256sum "$here/Dockerfile.edk2-ovmf-build" | awk '{print $1}')"

    # Reuse a versioned firmware artifact only when every recorded input and output digest still matches.
    if [[ -f "$out" ]] && [[ -f "$version_marker" ]] \
        && [[ "$(awk -F= '$1 == "edk2_version" { print $2 }' "$version_marker")" == "$edk2_version" ]] \
        && [[ "$(awk -F= '$1 == "edk2_commit" { print $2 }' "$version_marker")" == "$edk2_commit" ]] \
        && [[ "$(awk -F= '$1 == "ubuntu_snapshot" { print $2 }' "$version_marker")" == "$ubuntu_snapshot" ]] \
        && [[ "$(awk -F= '$1 == "debian_patch_version" { print $2 }' "$version_marker")" == "$debian_patch_version" ]] \
        && [[ "$(awk -F= '$1 == "debian_patch_sha256" { print $2 }' "$version_marker")" == "$debian_patch_sha256" ]] \
        && [[ "$(awk -F= '$1 == "build_input_sha256" { print $2 }' "$version_marker")" == "$build_input_sha256" ]] \
        && [[ "$(awk -F= '$1 == "firmware_sha256" { print $2 }' "$version_marker")" == "$expected_firmware_sha256" ]] \
        && [[ "$(sha256sum "$out" | awk '{print $1}')" == "$expected_firmware_sha256" ]]; then
        echo "$out already present and matches validated build inputs"
        return 0
    fi

    if ! command -v docker > /dev/null; then
        echo "Docker is required to build $edk2_version; install it before running $0" >&2
        exit 1
    fi

    mkdir -p "$out_dir"
    local image_tag="tdx-measure-edk2-ovmf:${suffix}"

    # Pass every version-specific pin to the shared Dockerfile rather than relying on mutable defaults.
    docker build \
        --build-arg "EDK2_VERSION=${edk2_version}" \
        --build-arg "EDK2_COMMIT=${edk2_commit}" \
        --build-arg "UBUNTU_SNAPSHOT=${ubuntu_snapshot}" \
        --build-arg "DEBIAN_PATCH_VERSION=${debian_patch_version}" \
        --build-arg "DEBIAN_PATCH_SHA256=${debian_patch_sha256}" \
        --tag "$image_tag" \
        --file "$here/Dockerfile.edk2-ovmf-build" \
        "$repo"

    # Copy the firmware from the minimal build image into the fixture-specific output directory.
    docker run --rm --user "$(id -u):$(id -g)" --volume "$out_dir:/output" "$image_tag"
    mv -f "$out_dir/OVMF.fd" "$out"

    # Ensure the newly built firmware is byte-for-byte identical to the accepted fixture artifact.
    local firmware_sha256
    firmware_sha256="$(sha256sum "$out" | awk '{print $1}')"
    if [[ "$firmware_sha256" != "$expected_firmware_sha256" ]]; then
        echo "$out sha256 mismatch: got $firmware_sha256, expected $expected_firmware_sha256" >&2
        exit 1
    fi

    # Record the complete verified identity required to reuse this generated artifact safely.
    {
        echo "edk2_version=$edk2_version"
        echo "edk2_commit=$edk2_commit"
        echo "ubuntu_snapshot=$ubuntu_snapshot"
        echo "debian_patch_version=$debian_patch_version"
        echo "debian_patch_sha256=$debian_patch_sha256"
        echo "build_input_sha256=$build_input_sha256"
        echo "firmware_sha256=$firmware_sha256"
    } > "$version_marker"
    echo "$out ready (built from $edk2_version, sha256=$firmware_sha256)"
}

# Prepare the shared firmware before processing every independently versioned upstream fixture.
if [[ "$mode" == "all" || "$mode" == "--shared-only" ]]; then
    ensure_shared_ovmf_fixture
fi
if [[ "$mode" == "all" || "$mode" == "--versioned-only" ]]; then
    versioned_builds='[
        {
            "edk2_version": "edk2-stable202502",
            "edk2_commit": "fbe0805b2091393406952e84724188f8c1941837",
            "ubuntu_snapshot": "20260903T000000Z",
            "debian_patch_version": "2025.11-3ubuntu7",
            "debian_patch_sha256": "543fb34c9be9b4a2846dc76d59657f7d47469bf42adf6e0801c95714791b9cfe",
            "expected_firmware_sha256": "b3a9f412e59dbaa4d183dbf270ffe9b4199c4c5d18530f581949825d22576f04"
        }
    ]'

    # Extract each named build input in the argument order consumed by the build function.
    while read -r edk2_version edk2_commit ubuntu_snapshot debian_patch_version debian_patch_sha256 expected_firmware_sha256; do
        ensure_versioned_ovmf_fixture "$edk2_version" "$edk2_commit" "$ubuntu_snapshot" "$debian_patch_version" "$debian_patch_sha256" "$expected_firmware_sha256"
    done < <(jq -r '.[] | [.edk2_version, .edk2_commit, .ubuntu_snapshot, .debian_patch_version, .debian_patch_sha256, .expected_firmware_sha256] | @tsv' <<< "$versioned_builds")
fi
