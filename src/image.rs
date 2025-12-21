/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * SPDX-License-Identifier: Apache-2.0
 */
use crate::{measure_log, measure_sha384, util::{debug_print_log, authenticode_sha384_hash}};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;
use log::debug;
use crate::util::utf16_encode;

/// Helper function to download a file using guestfish
fn guestfish_download(qcow2_path: &str, source_path: &str) -> Result<Vec<u8>> {

    // Create a temporary directory for the extracted files
    let temp_dir = std::env::temp_dir().join("tdx_bootloader_extract");
    std::fs::create_dir_all(&temp_dir)?;

    // Create a temporary file path
    let dest_path = temp_dir.join("extracted_file");

    // Download the file using guestfish
    let output = Command::new("guestfish")
        .args(&[
            "--ro", "-a", qcow2_path, "-i",
            "download", source_path, dest_path.to_str().unwrap()
        ])
        .output()
        .context(format!("Failed to extract {}", source_path))?;

    if !output.status.success() {
        bail!("Failed to extract {}: {}", source_path, String::from_utf8_lossy(&output.stderr));
    }

    // Read the extracted file
    let data = std::fs::read(&dest_path)
        .context(format!("Failed to read extracted {}", dest_path.to_str().unwrap()))?;

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(data)
}

/// Extracts GPT event data in the format used by EV_EFI_GPT_EVENT
fn extract_gpt_event_data(qcow2_path: &str) -> Result<Vec<u8>> {
    // Extract GPT header from LBA 1 (skip MBR at LBA 0)
    let output = Command::new("guestfish")
        .args(&[
            "--ro", "-a", qcow2_path,
            "run", ":",
            "pread-device", "/dev/sda", "512", "512"  // Read LBA 1 (GPT header)
        ])
        .output()
        .context("Failed to extract GPT header")?;

    if !output.status.success() {
        bail!("Failed to extract GPT header: {}", String::from_utf8_lossy(&output.stderr));
    }

    let gpt_header = output.stdout;

    // Parse GPT header
    if gpt_header.len() < 92 {
        bail!("GPT header too short");
    }

    // Check GPT signature
    if &gpt_header[0..8] != b"EFI PART" {
        bail!("Invalid GPT signature");
    }

    // Read partition entry info from GPT header
    let partition_entry_lba = u64::from_le_bytes(gpt_header[72..80].try_into().unwrap());
    let num_entries = u32::from_le_bytes(gpt_header[80..84].try_into().unwrap()) as usize;
    let entry_size = u32::from_le_bytes(gpt_header[84..88].try_into().unwrap()) as usize;

    // Calculate how many sectors we need to read for all partition entries
    let entries_size = num_entries * entry_size;
    let sectors_needed = (entries_size + 511) / 512; // Round up to sector boundary

    // Extract partition entries
    let entries_offset = partition_entry_lba * 512;
    let entries_length = sectors_needed * 512;

    let output = Command::new("guestfish")
        .args(&[
            "--ro", "-a", qcow2_path,
            "run", ":",
            "pread-device", "/dev/sda", &entries_length.to_string(), &entries_offset.to_string()
        ])
        .output()
        .context("Failed to extract GPT entries")?;

    if !output.status.success() {
        bail!("Failed to extract GPT entries: {}", String::from_utf8_lossy(&output.stderr));
    }

    let all_entries = output.stdout;

    // Filter valid partition entries (non-zero PartitionTypeGUID)
    let mut valid_entries = Vec::new();
    for i in 0..num_entries {
        let entry_offset = i * entry_size;
        if entry_offset + entry_size > all_entries.len() {
            break;
        }

        let entry = &all_entries[entry_offset..entry_offset + entry_size];

        // Check if PartitionTypeGUID is non-zero (first 16 bytes)
        let is_valid = !entry[0..16].iter().all(|&b| b == 0);

        if is_valid {
            valid_entries.push(entry.to_vec());
        }
    }

    // Build the EFI_GPT_DATA structure:
    // 1. GPT Header (92 bytes)
    // 2. NumberOfPartitions (8 bytes as UINT64)
    // 3. Valid partition entries (128 bytes each)
    let mut gpt_event_data = Vec::new();

    // Add GPT header (92 bytes)
    gpt_event_data.extend_from_slice(&gpt_header[0..92]);

    // Add NumberOfPartitions as 64-bit value
    let num_valid_partitions = valid_entries.len() as u64;
    gpt_event_data.extend_from_slice(&num_valid_partitions.to_le_bytes());

    // Add valid partition entries
    for entry in valid_entries {
        gpt_event_data.extend_from_slice(&entry);
    }

    Ok(gpt_event_data)
}

/// Reads the data from a file
fn read_file_data(filename: &str) -> Result<Vec<u8>> {
    let path = Path::new(filename);
    if path.exists() {
        match fs::read(path) {
            Ok(data) => {
                Ok(data)
            }
            Err(e) => {
                debug!("Failed to read {}: {}", filename, e);
                // Return empty data if file doesn't exist or can't be read
                Ok(Vec::new())
            }
        }
    } else {
        debug!("File {} not found, using empty data", filename);
        Ok(Vec::new())
    }
}

/// Extracts the kernel version from the command line BOOT_IMAGE parameter
fn extract_kernel_version_from_cmdline(cmdline: &str) -> Result<String> {
    // Look for BOOT_IMAGE parameter
    for param in cmdline.split_whitespace() {
        if param.starts_with("BOOT_IMAGE=") {
            let boot_image = param.strip_prefix("BOOT_IMAGE=").unwrap();
            // Extract version from vmlinuz filename
            // Example: /vmlinuz-6.8.0-60-generic -> 6.8.0-60-generic
            if let Some(version_start) = boot_image.find("vmlinuz-") {
                let version = &boot_image[version_start + 8..]; // Skip "vmlinuz-"
                if !version.is_empty() {
                    return Ok(version.to_string());
                }
            }
        }
    }
    bail!("Could not extract kernel version from command line: {}", cmdline);
}

/// Main function to measure RTMR1 from a qcow2 disk image
pub fn measure_rtmr1_from_qcow2(qcow2_path: &str) -> Result<Vec<u8>> {

    // Extract bootloader files
    let gpt_data = extract_gpt_event_data(qcow2_path)?;
    let shim_data = guestfish_download(qcow2_path, "/boot/efi/EFI/ubuntu/shimx64.efi")?;
    let grub_data = guestfish_download(qcow2_path, "/boot/efi/EFI/ubuntu/grubx64.efi")?;

    // Compute hashes of the bootloader components
    let gpt_hash = measure_sha384(&gpt_data);
    let shim_hash = authenticode_sha384_hash(&shim_data).context("Failed to compute shim hash")?;
    let grub_hash = authenticode_sha384_hash(&grub_data).context("Failed to compute grub hash")?;

    // Compute RTMR1 log
    let rtmr1_log = vec![
        measure_sha384(b"Calling EFI Application from Boot Option"),
        measure_sha384(&[0x00, 0x00, 0x00, 0x00]), // Separator
        gpt_hash,
        shim_hash,
        grub_hash,
        measure_sha384(b"Exit Boot Services Invocation"),
        measure_sha384(b"Exit Boot Services Returned with Success"),
    ];

    debug_print_log("RTMR1", &rtmr1_log);
    Ok(measure_log(&rtmr1_log))
}

/// Measures RTMR2 using actual MOK variable data extracted from shim
pub fn measure_rtmr2_from_qcow2(qcow2_path: &str, cmdline: &str, ref_mok_list: &str, ref_mok_list_trusted: &str, ref_mok_list_x: &str) -> Result<Vec<u8>> {

    // Extract reference MOK variables
    let ref_mok_list_data = read_file_data(ref_mok_list)?;
    let ref_mok_list_trusted_data = read_file_data(ref_mok_list_trusted)?;
    let ref_mok_list_x_data = read_file_data(ref_mok_list_x)?;

    // Extract kernel version from command line and construct initrd path
    let kernel_version = extract_kernel_version_from_cmdline(cmdline)?;
    let initrd_path = format!("/boot/initrd.img-{}", kernel_version);

    // Extract initrd
    let initrd_data = guestfish_download(qcow2_path, &initrd_path)?;

    // Compute RTMR2 log
    let rtmr2_log = vec![
        measure_sha384(&ref_mok_list_data),
        measure_sha384(&ref_mok_list_x_data),
        measure_sha384(&ref_mok_list_trusted_data),
        measure_sha384(&utf16_encode(cmdline)),
        measure_sha384(&initrd_data),
    ];

    debug_print_log("RTMR2", &rtmr2_log);
    Ok(measure_log(&rtmr2_log))
}
