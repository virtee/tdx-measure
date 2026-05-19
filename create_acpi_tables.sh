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
readonly SUPPORTED_DISTROS=("ubuntu:25.04")

# QEMU version mapping for each distribution
declare -A QEMU_VERSIONS
QEMU_VERSIONS["ubuntu:25.04"]="1:9.2.1+ds-1ubuntu4+tdx2.0~ppa2"

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

    # Optional generic QEMU shape descriptor. When `boot_config.qemu` is present,
    # the QEMU command is built from its fields verbatim (plus the seven measurement-
    # related core flags); nothing else is added implicitly. When absent, the script
    # falls back to the Canonical direct-boot defaults preserved from the upstream.
    QEMU_BLOCK_PRESENT=$(jq -r '.boot_config.qemu // empty' "$METADATA_JSON_PATH")
    if [[ -n "$QEMU_BLOCK_PRESENT" ]]; then
        QEMU_MACHINE=$(jq -r '.boot_config.qemu.machine // empty' "$METADATA_JSON_PATH")
        QEMU_CPU=$(jq -r '.boot_config.qemu.cpu // "host"' "$METADATA_JSON_PATH")
        QEMU_ACCEL=$(jq -r '.boot_config.qemu.accel // "kvm"' "$METADATA_JSON_PATH")
        if [[ -z "$QEMU_MACHINE" ]]; then
            log_error "boot_config.qemu is present but boot_config.qemu.machine is missing"
            exit 1
        fi
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
    if [[ -n "$QEMU_BLOCK_PRESENT" ]]; then
        log_info "Using boot_config.qemu shape: machine='$QEMU_MACHINE' cpu='$QEMU_CPU' accel='$QEMU_ACCEL'"
    fi
}

# Build Docker image
build_docker_image() {
    log_info "Building Docker image: $IMAGE_NAME"

    # Get path to Dockerfile
    local DOCKERFILE_PATH="$SCRIPT_DIR/Dockerfile.qemu-acpi-dump"

    # Get QEMU version for the distribution
    local QEMU_VERSION="${QEMU_VERSIONS[$DISTRIBUTION]}"
    if [[ -z "$QEMU_VERSION" ]]; then
        log_error "No QEMU version defined for distribution: $DISTRIBUTION"
        exit 1
    fi
    log_info "Using QEMU version: $QEMU_VERSION"

    # Build Docker image
    if ! docker build \
        --progress plain \
        --tag "$IMAGE_NAME" \
        --build-arg "DISTRIBUTION=$DISTRIBUTION" \
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

# Build the QEMU command from the `boot_config.qemu` shape descriptor in metadata.json.
# The seven measurement-related core flags are added unconditionally; everything
# else comes verbatim from the block. Within `devices`, command-line order is
# preserved because QEMU's PCI auto-slot assignment depends on it.
build_qemu_args_from_block() {
    local out_var="$1"
    local -a args=(
        "-accel" "$QEMU_ACCEL"
        "-m" "$MEMORY"
        "-smp" "${CPUS},maxcpus=${CPUS}"
        "-cpu" "$QEMU_CPU"
        "-no-reboot"
        "-nodefaults"
        "-vga" "none"
        "-nographic"
        "-bios" "/usr/share/ovmf/OVMF.fd"
        "-machine" "$QEMU_MACHINE"
    )
    local v
    while IFS= read -r v; do [[ -n "$v" ]] && args+=("-global" "$v"); done \
        < <(jq -r '.boot_config.qemu.globals[]? // empty' "$METADATA_JSON_PATH")
    while IFS= read -r v; do [[ -n "$v" ]] && args+=("-object" "$v"); done \
        < <(jq -r '.boot_config.qemu.objects[]? // empty' "$METADATA_JSON_PATH")
    while IFS= read -r v; do [[ -n "$v" ]] && args+=("-netdev" "$v"); done \
        < <(jq -r '.boot_config.qemu.netdevs[]? // empty' "$METADATA_JSON_PATH")
    while IFS= read -r v; do [[ -n "$v" ]] && args+=("-device" "$v"); done \
        < <(jq -r '.boot_config.qemu.devices[]? // empty' "$METADATA_JSON_PATH")
    while IFS= read -r v; do [[ -n "$v" ]] && args+=("-fw_cfg" "$v"); done \
        < <(jq -r '.boot_config.qemu.fw_cfg[]? // empty' "$METADATA_JSON_PATH")
    # We can't directly assign an array to a nameref-by-string in older bash,
    # so eval the assignment via printf-quoted args.
    eval "${out_var}=(\"\${args[@]}\")"
}

# Run QEMU container to generate ACPI tables
generate_acpi_tables() {
    log_info "Running QEMU container to generate ACPI tables..."
    log_warning "NOTE: You may see ROM file errors (kvmvapic.bin, linuxboot_dma.bin) - these can be safely ignored."
    log_warning "This script uses QEMU source from ppa:kobuk-team/tdx-release PPA which provides Intel TDX-enabled QEMU."
    log_warning "Some upstream BIOS files are missing in this QEMU source but are not required for ACPI table generation."

    local qemu_args=()
    if [[ -n "$QEMU_BLOCK_PRESENT" ]]; then
        log_info "Building QEMU args from boot_config.qemu"
        build_qemu_args_from_block qemu_args
    else
        # Canonical direct-boot defaults: minimal args from
        # https://github.com/canonical/tdx/blob/3.3/guest-tools/direct-boot/boot_direct.sh#L54
        qemu_args=(
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
    fi

    # Create temporary directory for ACPI tables output to allow the user in the container to write to it.
    local tmp_acpi_tables_dir
    tmp_acpi_tables_dir=$(mktemp -d)
    chmod o+rwx "$tmp_acpi_tables_dir"

    # Whether we need to expose /dev/kvm. The Canonical fallback path always uses
    # KVM (its hardcoded args contain `-accel kvm`). For the qemu-block path, we
    # mount /dev/kvm only when the caller asked for `accel: "kvm"` — `tcg` (or
    # any other software backend) lifts the "x86 host with KVM" requirement.
    local need_kvm=true
    if [[ -n "$QEMU_BLOCK_PRESENT" && "$QEMU_ACCEL" != "kvm" ]]; then
        need_kvm=false
    fi

    # The kvm group also owns /dev/vhost-vsock on Linux, so add it whenever
    # either device matters.
    local extra_docker_args=()
    local kvm_gid
    kvm_gid=$(getent group kvm | cut -d: -f3 || true)
    if [[ -n "$kvm_gid" ]]; then
        extra_docker_args+=(--group-add "$kvm_gid")
    fi

    if $need_kvm; then
        if [[ ! -e /dev/kvm ]]; then
            log_error "Accelerator requires /dev/kvm but the host has none (use accel: \"tcg\" to skip KVM)"
            exit 1
        fi
        extra_docker_args+=(--device /dev/kvm:/dev/kvm)
    fi

    # Some QEMU device strings (e.g. `vhost-vsock-pci`) need /dev/vhost-vsock
    # from the host. Mount it whenever a custom qemu block is in play and the
    # host has the device; unused if the block doesn't reference it.
    if [[ -n "$QEMU_BLOCK_PRESENT" && -e /dev/vhost-vsock ]]; then
        extra_docker_args+=(--device /dev/vhost-vsock:/dev/vhost-vsock)
    fi

    # Run Docker container
    if ! docker run \
        --rm \
        --name "$CONTAINER_NAME" \
        "${extra_docker_args[@]}" \
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
