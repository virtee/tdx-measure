/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * SPDX-License-Identifier: Apache-2.0
 */
use log::debug;
use sha2::{Digest, Sha384};
use crate::{num::read_le};
use anyhow::{bail, Result};
use object::pe;
use std::fs;

/// Computes a SHA384 hash of the given data.
pub(crate) fn measure_sha384(data: &[u8]) -> Vec<u8> {
    Sha384::new_with_prefix(data).finalize().to_vec()
}

pub(crate) fn utf16_encode(input: &str) -> Vec<u8> {
    input
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes().into_iter())
        .collect()
}

pub(crate) fn debug_print_log(name: &str, log: &[Vec<u8>]) {
    debug!("{name} event log:");
    for (i, entry) in log.iter().enumerate() {
        debug!("[{i}] digest: {}", hex::encode(entry));
    }
}

/// Reads file data and returns the data or fails with an error
pub(crate) fn read_file_data(filename: &str) -> Result<Vec<u8>> {
    match fs::read(filename) {
        Ok(data) => {
            debug!("Loaded {} bytes from {}", data.len(), filename);
            Ok(data)
        }
        Err(e) => {
            bail!("Failed to load file {}: {}", filename, e);
        }
    }
}

/// Computes a measurement of the given RTMR event log.
pub(crate) fn measure_log(log: &[Vec<u8>]) -> Vec<u8> {
    let mut mr = [0u8; 48]; // SHA384 output size
    for entry in log {
        let mut hasher = Sha384::new();
        hasher.update(mr);
        hasher.update(entry);
        mr = hasher.finalize().into();
    }
    mr.to_vec()
}

/// Measures the kernel command line by converting to UTF-16LE and hashing.
pub(crate) fn measure_cmdline(cmdline: &str) -> Vec<u8> {
    let mut utf16_cmdline = utf16_encode(cmdline);
    utf16_cmdline.extend([0, 0]);
    measure_sha384(&utf16_cmdline)
}

/// Calculates the Authenticode hash of a PE/COFF file
pub(crate) fn authenticode_sha384_hash(data: &[u8]) -> Result<Vec<u8>> {
    let lfanew_offset = 0x3c;
    let lfanew: u32 = read_le(data, lfanew_offset, "DOS header")?;

    let pe_sig_offset = lfanew as usize;
    let pe_sig: u32 = read_le(data, pe_sig_offset, "PE signature offset")?;
    if pe_sig != pe::IMAGE_NT_SIGNATURE {
        bail!("Invalid PE signature");
    }

    let coff_header_offset = pe_sig_offset + 4;
    let optional_header_size =
        read_le::<u16>(data, coff_header_offset + 16, "COFF header size")? as usize;

    let optional_header_offset = coff_header_offset + 20;
    let magic: u16 = read_le(data, optional_header_offset, "header magic")?;

    let is_pe32_plus = magic == 0x20b;

    let checksum_offset = optional_header_offset + 64;
    let checksum_end = checksum_offset + 4;

    let data_dir_offset = optional_header_offset + if is_pe32_plus { 112 } else { 96 };
    let cert_dir_offset = data_dir_offset + (pe::IMAGE_DIRECTORY_ENTRY_SECURITY * 8);
    let cert_dir_end = cert_dir_offset + 8;

    let size_of_headers_offset = optional_header_offset + 60;
    let size_of_headers = read_le::<u32>(data, size_of_headers_offset, "size_of_headers")? as usize;

    let mut hasher = Sha384::new();
    hasher.update(&data[0..checksum_offset]);
    hasher.update(&data[checksum_end..cert_dir_offset]);
    hasher.update(&data[cert_dir_end..size_of_headers]);

    let mut sum_of_bytes_hashed = size_of_headers;

    let num_sections_offset = coff_header_offset + 2;
    let num_sections = read_le::<u16>(data, num_sections_offset, "number of sections")? as usize;

    let section_table_offset = optional_header_offset + optional_header_size;
    let section_size = 40;

    let mut sections = Vec::with_capacity(num_sections);
    for i in 0..num_sections {
        let section_offset = section_table_offset + (i * section_size);

        let ptr_raw_data_offset = section_offset + 20;
        let ptr_raw_data =
            read_le::<u32>(data, ptr_raw_data_offset, "pointer_to_raw_data")? as usize;

        let size_raw_data_offset = section_offset + 16;
        let size_raw_data =
            read_le::<u32>(data, size_raw_data_offset, "size_of_raw_data")? as usize;

        if size_raw_data > 0 {
            sections.push((ptr_raw_data, size_raw_data));
        }
    }

    sections.sort_by_key(|&(offset, _)| offset);

    for (offset, size) in sections {
        let start = offset;
        let end = start + size;

        if end <= data.len() {
            hasher.update(&data[start..end]);
        } else {
            let available_size = data.len().saturating_sub(start);
            if available_size > 0 {
                hasher.update(&data[start..start + available_size]);
            }
        }

        sum_of_bytes_hashed += size;
    }

    let file_size = data.len();

    let cert_table_addr_offset = cert_dir_offset;
    let cert_table_size_offset = cert_dir_offset + 4;

    let cert_table_addr =
        read_le::<u32>(data, cert_table_addr_offset, "certificate table address")? as usize;
    let cert_table_size =
        read_le::<u32>(data, cert_table_size_offset, "certificate table size")? as usize;

    if cert_table_addr > 0 && cert_table_size > 0 && file_size > sum_of_bytes_hashed {
        let trailing_data_len = file_size - sum_of_bytes_hashed;

        if trailing_data_len > cert_table_size {
            let hashed_trailing_len = trailing_data_len - cert_table_size;
            let trailing_start = sum_of_bytes_hashed;

            if trailing_start + hashed_trailing_len <= data.len() {
                hasher.update(&data[trailing_start..trailing_start + hashed_trailing_len]);
            }
        }
    }
    let remainder = file_size % 8;
    if remainder != 0 {
        let padding = vec![0u8; 8 - remainder];
        hasher.update(&padding);
    }
    Ok(hasher.finalize().to_vec())
}
