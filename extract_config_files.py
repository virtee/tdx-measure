#!/usr/bin/env python3
# Copyright (c) 2025 Tinfoil Inc
# SPDX-License-Identifier: Apache-2.0
import os
import sys
import struct

def extract_boot_variable(var_name, output_file):
    efi_var_path = f"/sys/firmware/efi/efivars/{var_name}-8be4df61-93ca-11d2-aa0d-00e098032b8c"

    if not os.path.exists(efi_var_path):
        print(f"Error: {var_name} variable not found at {efi_var_path}")
        return False

    try:
        with open(efi_var_path, 'rb') as f:
            raw_data = f.read()

        # Skip first 4 bytes (attributes)
        variable_data = raw_data[4:]

        with open(output_file, 'wb') as f:
            f.write(variable_data)

        print(f"Extracted {var_name} variable data: {len(variable_data)} bytes -> {output_file}")

        if len(variable_data) > 0:
            # Show first 16 bytes
            hex_preview = ' '.join(f'{b:02x}' for b in variable_data[:16])
            print(f"First 16 bytes: {hex_preview}")

        return True

    except Exception as e:
        print(f"Error extracting {var_name}: {e}")
        return False

def parse_boot_order(boot_order_data):
    """Parse BootOrder variable to extract boot entry numbers"""
    if len(boot_order_data) % 2 != 0:
        raise ValueError("BootOrder data length must be even (array of UINT16s)")

    boot_entries = []
    for i in range(0, len(boot_order_data), 2):
        # Extract UINT16 in little-endian format
        entry_num = struct.unpack('<H', boot_order_data[i:i+2])[0]
        boot_entries.append(entry_num)

    return boot_entries

def extract_mok_variable(var_name, output_file):
    """Extract MOK variable from /sys/firmware/efi/mok-variables/"""
    mok_var_path = f"/sys/firmware/efi/mok-variables/{var_name}"

    if not os.path.exists(mok_var_path):
        print(f"Error: {var_name} MOK variable not found at {mok_var_path}")
        return False

    try:
        with open(mok_var_path, 'rb') as f:
            variable_data = f.read()

        with open(output_file, 'wb') as f:
            f.write(variable_data)

        print(f"Extracted {var_name} MOK variable data: {len(variable_data)} bytes -> {output_file}")

        if len(variable_data) > 0:
            # Show first 16 bytes
            hex_preview = ' '.join(f'{b:02x}' for b in variable_data[:16])
            print(f"First 16 bytes: {hex_preview}")

        return True

    except Exception as e:
        print(f"Error extracting {var_name}: {e}")
        return False

def extract_acpi_data(source_path, output_file, description):
    """Extract ACPI data from QEMU fw_cfg interface"""
    if not os.path.exists(source_path):
        print(f"Error: {description} not found at {source_path}")
        return False

    try:
        with open(source_path, 'rb') as f:
            data = f.read()

        with open(output_file, 'wb') as f:
            f.write(data)

        print(f"Extracted {description}: {len(data)} bytes -> {output_file}")

        if len(data) > 0:
            # Show first 16 bytes
            hex_preview = ' '.join(f'{b:02x}' for b in data[:16])
            print(f"First 16 bytes: {hex_preview}")

        return True

    except Exception as e:
        print(f"Error extracting {description}: {e}")
        return False

def main():
    if os.geteuid() != 0:
        print("This script must be run as root to access EFI variables and ACPI data")
        sys.exit(1)

    print("Extracting Boot000X EFI variable data...")

    # Extract BootOrder and parse it to get boot entry numbers
    boot_order_var = "BootOrder"
    boot_order_file = boot_order_var + ".bin"
    if not extract_boot_variable(boot_order_var, boot_order_file):
        print("No boot order found, falling back to common boot variables")
        boot_entries = [0x0000, 0x0001, 0x0006, 0x0007]  # Common fallback entries
    else:
        with open(boot_order_file, 'rb') as f:
            boot_order_data = f.read()
        boot_entries = parse_boot_order(boot_order_data)

    print(f"\nExtracting Boot variables for entries: {[f'Boot{entry:04X}' for entry in boot_entries]}")

    # Extract boot variables dynamically based on BootOrder
    extracted_variables = [boot_order_var]  # BootOrder is always extracted
    for boot_entry_num in boot_entries:
        boot_entry_var = f"Boot{boot_entry_num:04X}"
        if extract_boot_variable(boot_entry_var, boot_entry_var + ".bin"):
            extracted_variables.append(boot_entry_var)

    print("\nExtracting MOK variables...")

    # MOK variable extractions
    mok_variables = ["MokListRT", "MokListTrustedRT", "MokListXRT", "SbatLevelRT"]

    for var_name in mok_variables:
        output_file = f"{var_name}.bin"
        extract_mok_variable(var_name, output_file)

    print("\nExtracting ACPI data...")

    # ACPI data extractions
    acpi_extractions = [
        ("/sys/firmware/qemu_fw_cfg/by_name/etc/acpi/tables/raw", "acpi_tables.bin", "ACPI tables"),
        ("/sys/firmware/qemu_fw_cfg/by_name/etc/acpi/rsdp/raw", "rsdp.bin", "RSDP"),
        ("/sys/firmware/qemu_fw_cfg/by_name/etc/table-loader/raw", "table_loader.bin", "Table loader")
    ]

    for source_path, output_file, description in acpi_extractions:
        extract_acpi_data(source_path, output_file, description)

    print("\nDone!")
    print("\nFiles created:")

    # Show EFI variable files
    for var_name in extracted_variables:
        output_file = f"{var_name}.bin"
        if os.path.exists(output_file):
            size = os.path.getsize(output_file)
            print(f"  {output_file}: {size} bytes")

    # Show MOK variable files
    for var_name in mok_variables:
        output_file = f"{var_name}.bin"
        if os.path.exists(output_file):
            size = os.path.getsize(output_file)
            print(f"  {output_file}: {size} bytes")

    # Show ACPI files
    for _, output_file, _ in acpi_extractions:
        if os.path.exists(output_file):
            size = os.path.getsize(output_file)
            print(f"  {output_file}: {size} bytes")

if __name__ == "__main__":
    main()
