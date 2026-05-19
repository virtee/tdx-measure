#!/bin/bash
# Copyright (c) 2025-2026 Intel Corporation
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Global variables
SCRIPT_NAME=$(basename "$0")
readonly SCRIPT_NAME
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
readonly CONTAINER_NAME="acpi-tables-generator"
readonly IMAGE_NAME="acpi-tables-generator"

# Supported distributions
readonly SUPPORTED_DISTROS=("ubuntu:25.04" "ubuntu:26.04")

# QEMU source + version mapping per distribution.
# - ubuntu:25.04: Intel TDX-enabled QEMU 9.2.1 from ppa:kobuk-team/tdx-release
# - ubuntu:26.04: stock QEMU 10.2.x from Ubuntu's main archive (includes the CEJL
#   multi-eject hotplug feature that newer guest ACPI tables expect).
declare -A QEMU_SOURCES
declare -A QEMU_VERSIONS
QEMU_SOURCES["ubuntu:25.04"]="ppa"
QEMU_VERSIONS["ubuntu:25.04"]="1:9.2.1+ds-1ubuntu4+tdx2.0~ppa2"
QEMU_SOURCES["ubuntu:26.04"]="main"
QEMU_VERSIONS["ubuntu:26.04"]=""  # let pull-lp-source pick the current main-archive version

# Color codes for output
readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*" >&2
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $*" >&2
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $*" >&2
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

# Display usage information
usage() {
    cat <<EOF
Usage: ${SCRIPT_NAME} [OPTIONS]

Generate ACPI tables using QEMU in a Docker container for TDX measurements.

OPTIONS:
    -j <PATH>   Path to the metadata.json configuration file (required)
    -d <DISTRO> Docker base image distribution (required)
                Supported distributions: ubuntu:25.04
    -h          Display this help message

EXAMPLES:
    ./${SCRIPT_NAME} -j metadata.json -d ubuntu:25.04
EOF
}

# Validate command line arguments
validate_args() {
    local has_errors=false

    if [[ -z "${METADATA_JSON_PATH:-}" ]]; then
        log_error "Missing required argument: -j <METADATA_JSON_PATH>"
        has_errors=true
    elif [[ ! -f "$METADATA_JSON_PATH" ]]; then
        log_error "Metadata file not found: $METADATA_JSON_PATH"
        has_errors=true
    fi

    if [[ -z "${DISTRIBUTION:-}" ]]; then
        log_error "Missing required argument: -d <DISTRIBUTION>"
        has_errors=true
    else
        # Validate distribution is supported
        local distro_supported=false
        local supported_distro
        for supported_distro in "${SUPPORTED_DISTROS[@]}"; do
            if [[ "$DISTRIBUTION" == "$supported_distro" ]]; then
                distro_supported=true
                break
            fi
        done

        if [[ "$distro_supported" == "false" ]]; then
            log_error "Unsupported distribution: $DISTRIBUTION"
            log_error "Supported distributions: ${SUPPORTED_DISTROS[*]}"
            has_errors=true
        fi
    fi

    if [[ "$has_errors" == "true" ]]; then
        usage
        exit 1
    fi
}

# Process command line arguments
process_args() {
    # Check if no arguments provided
    if [[ $# -eq 0 ]]; then
        log_error "No arguments provided"
        usage
        exit 1
    fi

    # Parse arguments
    local option
    while getopts "j:d:h" option; do
        case "$option" in
            j) METADATA_JSON_PATH="$OPTARG" ;;
            d) DISTRIBUTION="$OPTARG" ;;
            h) usage; exit 0 ;;
            *)
                log_error "Invalid option: -$OPTARG"
                usage
                exit 1
                ;;
        esac
    done

    validate_args
}

# Extract and validate a required field from JSON
extract_and_validate() {
    local var_name="$1"
    local jq_expr="$2"
    local value
    value=$(jq -r "$jq_expr // empty" "$METADATA_JSON_PATH")
    if [[ -z "$value" ]]; then
        log_error "Missing or null: $jq_expr"
        return 1
    fi
    printf -v "$var_name" '%s' "$value"
    return 0
}

# Parse metadata JSON configuration
parse_metadata() {
    log_info "Parsing metadata configuration from: $METADATA_JSON_PATH"

    # Validate JSON syntax
    if ! jq empty "$METADATA_JSON_PATH" 2>/dev/null; then
        log_error "Invalid JSON syntax in: $METADATA_JSON_PATH"
        exit 1
    fi

    # Get the directory containing the metadata file for resolving relative paths
    local metadata_dir
    metadata_dir="$(cd "$(dirname "$METADATA_JSON_PATH")" && pwd)"

    # Read settings from metadata JSON file
    local has_errors=false
    extract_and_validate CPUS             '.boot_config.cpus'          || has_errors=true
    extract_and_validate MEMORY           '.boot_config.memory'        || has_errors=true
    extract_and_validate BIOS             '.boot_config.bios'          || has_errors=true
    extract_and_validate ACPI_TABLES_PATH '.boot_config.acpi_tables'   || has_errors=true

    if [[ "$has_errors" == "true" ]]; then
        log_error "Metadata validation failed"
        exit 1
    fi

    # Resolve paths relative to metadata.json location (if not already absolute)
    [[ "$BIOS" != /* ]] && BIOS="$metadata_dir/$BIOS"
    [[ "$ACPI_TABLES_PATH" != /* ]] && ACPI_TABLES_PATH="$metadata_dir/$ACPI_TABLES_PATH"

    # Check if target directory for ACPI tables exists
    if [[ ! -d "$(dirname "$ACPI_TABLES_PATH")" ]]; then
        log_error "Target directory for ACPI tables does not exist: $(dirname "$ACPI_TABLES_PATH")"
        exit 1
    fi

    ACPI_TABLES_PATH="$(realpath "$ACPI_TABLES_PATH")"
    BIOS="$(realpath "$BIOS")"

    # Validate that BIOS file exists
    if [[ ! -f "$BIOS" ]]; then
        log_error "BIOS file not found: $BIOS"
        exit 1
    fi

    log_success "Metadata parsed successfully"
    log_info "Configuration: CPUs=$CPUS, Memory=$MEMORY, BIOS=$BIOS, ACPI Tables Target Path=$ACPI_TABLES_PATH"
}

# Build Docker image
build_docker_image() {
    log_info "Building Docker image: $IMAGE_NAME"

    # Get path to Dockerfile
    local DOCKERFILE_PATH="$SCRIPT_DIR/Dockerfile.qemu-acpi-dump"

    local QEMU_SOURCE="${QEMU_SOURCES[$DISTRIBUTION]}"
    local QEMU_VERSION="${QEMU_VERSIONS[$DISTRIBUTION]}"
    if [[ -z "$QEMU_SOURCE" ]]; then
        log_error "No QEMU source defined for distribution: $DISTRIBUTION"
        exit 1
    fi
    case "$QEMU_SOURCE" in
        ppa)  log_info "QEMU source: ppa:kobuk-team/tdx-release (Intel TDX-patched QEMU ${QEMU_VERSION}) on ${DISTRIBUTION}" ;;
        main) log_info "QEMU source: ${DISTRIBUTION} main archive (${QEMU_VERSION:-latest})" ;;
        *)    log_info "QEMU source: $QEMU_SOURCE (${QEMU_VERSION:-?})" ;;
    esac

    # Build Docker image
    if ! docker build \
        --progress plain \
        --tag "$IMAGE_NAME" \
        --build-arg "DISTRIBUTION=$DISTRIBUTION" \
        --build-arg "QEMU_SOURCE=$QEMU_SOURCE" \
        --build-arg "QEMU_VERSION=$QEMU_VERSION" \
        --build-arg "USER=$USER" \
        --build-arg "ACPI_TABLES_NAME=$(basename "$ACPI_TABLES_PATH")" \
        --file "$DOCKERFILE_PATH" \
        "$SCRIPT_DIR"; then
        log_error "Docker build failed"
        exit 1
    fi

    log_success "Docker image built successfully"
}

# Run QEMU container to generate ACPI tables
generate_acpi_tables() {
    log_info "Running QEMU container to generate ACPI tables..."
    # The Dockerfile skips `subdir('pc-bios')` in QEMU's meson build (the option
    # ROMs aren't needed to dump ACPI tables and adding them ~doubles image size).
    # As a result QEMU prints "rom: file kvmvapic.bin … No such file or directory"
    # (and similar for `linuxboot_dma.bin` if a PCI NIC is in play) on every run.
    # They are harmless — the ACPI dump runs before any ROM would be loaded.
    log_warning "NOTE: harmless ROM-file errors (kvmvapic.bin, linuxboot_dma.bin, ...) from QEMU are expected; pc-bios is skipped in the patched build."

    # Construct QEMU arguments using minimal QEMU arguments from Canonical's direct boot script
    # https://github.com/canonical/tdx/blob/3.3/guest-tools/direct-boot/boot_direct.sh#L54
    # to match ACPI event measurements
    local qemu_args=(
        "-accel" "kvm"
        "-m" "$MEMORY"
        "-smp" "$CPUS"
        "-cpu" "host"
        "-machine" "q35,kernel-irqchip=split,hpet=off,smm=off,pic=off"
        "-bios" "/usr/share/ovmf/OVMF.fd"
        "-nographic"
        "-nodefaults"
        "-serial" "stdio"
    )

    # Get host's kvm group ID for device access
    local kvm_gid
    kvm_gid=$(getent group kvm | cut -d: -f3)
    if [[ -z "$kvm_gid" ]]; then
        log_error "kvm group not found on host system"
        exit 1
    fi

    # Create temporary directory for ACPI tables output to allow the user in the container to write to it.
    local tmp_acpi_tables_dir
    tmp_acpi_tables_dir=$(mktemp -d)
    chmod o+rwx "$tmp_acpi_tables_dir"

    # Run Docker container
    if ! docker run \
        --rm \
        --name "$CONTAINER_NAME" \
        --device /dev/kvm:/dev/kvm \
        --group-add "$kvm_gid" \
        -v "$BIOS:/usr/share/ovmf/OVMF.fd:ro" \
        -v "$tmp_acpi_tables_dir:/output" \
        "$IMAGE_NAME" \
        "${qemu_args[@]}"; then
        log_error "QEMU execution failed"
        exit 1
    fi

    # Copy ACPI tables file from temporary directory to the expected path with proper permissions.
    local tmp_acpi_tables_file
    tmp_acpi_tables_file="$tmp_acpi_tables_dir/$(basename "$ACPI_TABLES_PATH")"
    if [[ -f "$tmp_acpi_tables_file" ]]; then
        sudo chmod o+rw "$tmp_acpi_tables_file"
        cp "$tmp_acpi_tables_file" "$(dirname "$ACPI_TABLES_PATH")"
        log_success "ACPI tables from temporary directory copied to: $ACPI_TABLES_PATH"
    else
        log_error "ACPI tables not found in temporary output: $tmp_acpi_tables_file"
        exit 1
    fi

    # Clean up temporary ACPI tables directory.
    rm -rf "$tmp_acpi_tables_dir"

    log_success "QEMU container executed successfully"
    log_info "QEMU command: ${qemu_args[*]}"
}

# Main execution function
main() {
    log_info "Starting ACPI tables generation process..."

    # Execute main workflow
    process_args "$@"
    parse_metadata
    build_docker_image
    generate_acpi_tables

    # Verify if ACPI tables file is created
    if [[ -f "$ACPI_TABLES_PATH" ]]; then
        sudo chown "$USER" "$ACPI_TABLES_PATH"
        log_success "ACPI tables created successfully at: $ACPI_TABLES_PATH"
    else
        log_error "ACPI tables not found at: $ACPI_TABLES_PATH"
        exit 1
    fi
}

# Execute main function with all arguments
main "$@"
