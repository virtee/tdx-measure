/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * SPDX-License-Identifier: Apache-2.0
 */
//! This module provides functionality to load ACPI tables for QEMU from files.

use anyhow::{bail, Result};

use crate::util::read_file_data;
use crate::Machine;

const LDR_LENGTH: usize = 4096;
const FIXED_STRING_LEN: usize = 56;

pub struct Tables {
    pub tables: Vec<u8>,
    pub rsdp: Vec<u8>,
    pub loader: Vec<u8>,
}

impl Machine<'_> {
    pub fn build_tables(&self) -> Result<Tables> {
        let tables  = read_file_data(self.acpi_tables)?;

        let rsdp: Vec<u8> = if !self.rsdp.is_empty() {
            read_file_data(self.rsdp)?
        } else {
            let (rsdt_offset, _rsdt_csum, _rsdt_len) = find_acpi_table(&tables , "RSDT")?;

            // Generate RSDP
            let mut rsdp = Vec::with_capacity(20);
            rsdp.extend_from_slice(b"RSD PTR "); // Signature
            rsdp.push(0x00); // Checksum placeholder
            rsdp.extend_from_slice(b"BOCHS "); // OEM ID
            rsdp.push(0x00); // Revision
            rsdp.extend_from_slice(&rsdt_offset.to_le_bytes()); // RSDT Address
            rsdp
        };

        let loader: Vec<u8> = if !self.table_loader.is_empty() {
            read_file_data(self.table_loader)?
        } else {
            let (dsdt_offset, dsdt_csum, dsdt_len) = find_acpi_table(&tables , "DSDT")?;
            let (facp_offset, facp_csum, facp_len) = find_acpi_table(&tables , "FACP")?;
            let (apic_offset, apic_csum, apic_len) = find_acpi_table(&tables , "APIC")?;
            let (mcfg_offset, mcfg_csum, mcfg_len) = find_acpi_table(&tables , "MCFG")?;
            let (waet_offset, waet_csum, waet_len) = find_acpi_table(&tables , "WAET")?;
            let (rsdt_offset, rsdt_csum, rsdt_len) = find_acpi_table(&tables , "RSDT")?;
            let mut loader: TableLoader = TableLoader::new();
            loader.append(LoaderCmd::Allocate {
                file: "etc/acpi/rsdp",
                alignment: 16,
                zone: 2,
            });
            loader.append(LoaderCmd::Allocate {
                file: "etc/acpi/tables",
                alignment: 64,
                zone: 1,
            });
            loader.append(LoaderCmd::AddChecksum {
                file: "etc/acpi/tables",
                result_offset: dsdt_csum,
                start: dsdt_offset,
                length: dsdt_len,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/tables",
                pointee_file: "etc/acpi/tables",
                pointer_offset: facp_offset + 36,
                pointer_size: 4,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/tables",
                pointee_file: "etc/acpi/tables",
                pointer_offset: facp_offset + 40,
                pointer_size: 4,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/tables",
                pointee_file: "etc/acpi/tables",
                pointer_offset: facp_offset + 140,
                pointer_size: 8,
            });
            loader.append(LoaderCmd::AddChecksum {
                file: "etc/acpi/tables",
                result_offset: facp_csum,
                start: facp_offset,
                length: facp_len,
            });
            loader.append(LoaderCmd::AddChecksum {
                file: "etc/acpi/tables",
                result_offset: apic_csum,
                start: apic_offset,
                length: apic_len,
            });
            loader.append(LoaderCmd::AddChecksum {
                file: "etc/acpi/tables",
                result_offset: mcfg_csum,
                start: mcfg_offset,
                length: mcfg_len,
            });
            loader.append(LoaderCmd::AddChecksum {
                file: "etc/acpi/tables",
                result_offset: waet_csum,
                start: waet_offset,
                length: waet_len,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/tables",
                pointee_file: "etc/acpi/tables",
                pointer_offset: rsdt_offset + 36,
                pointer_size: 4,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/tables",
                pointee_file: "etc/acpi/tables",
                pointer_offset: rsdt_offset + 40,
                pointer_size: 4,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/tables",
                pointee_file: "etc/acpi/tables",
                pointer_offset: rsdt_offset + 44,
                pointer_size: 4,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/tables",
                pointee_file: "etc/acpi/tables",
                pointer_offset: rsdt_offset + 48,
                pointer_size: 4,
            });
            loader.append(LoaderCmd::AddChecksum {
                file: "etc/acpi/tables",
                result_offset: rsdt_csum,
                start: rsdt_offset,
                length: rsdt_len,
            });
            loader.append(LoaderCmd::AddPtr {
                pointer_file: "etc/acpi/rsdp",
                pointee_file: "etc/acpi/tables",
                pointer_offset: 16,
                pointer_size: 4,
            });
            loader.append(LoaderCmd::AddChecksum {
                file: "etc/acpi/rsdp",
                result_offset: 8,
                start: 0,
                length: 20,
            });
            if loader.buffer.len() < LDR_LENGTH {
                loader.buffer.resize(LDR_LENGTH, 0);
            }
            loader.buffer
        };

        Ok(Tables {
            tables,
            rsdp,
            loader,
        })
    }
}

/// An enum to represent the different QEMU loader commands in a type-safe way.
#[derive(Debug)]
enum LoaderCmd<'a> {
    Allocate {
        file: &'a str,
        alignment: u32,
        zone: u8,
    },
    AddPtr {
        pointer_file: &'a str,
        pointee_file: &'a str,
        pointer_offset: u32,
        pointer_size: u8,
    },
    AddChecksum {
        file: &'a str,
        result_offset: u32,
        start: u32,
        length: u32,
    },
}

/// Builder for QEMU-specific loader commands that instruct firmware how to load and patch ACPI tables.
struct TableLoader {
    /// Buffer containing serialized QEMU loader commands
    buffer: Vec<u8>,
}

impl TableLoader {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(LDR_LENGTH),
        }
    }

    /// Appends a fixed-length, null-padded string to the data buffer.
    fn append_fixed_string(data: &mut Vec<u8>, s: &str) {
        let mut s_bytes = s.as_bytes().to_vec();
        s_bytes.resize(FIXED_STRING_LEN, 0);
        data.extend_from_slice(&s_bytes);
    }

    fn append(&mut self, cmd: LoaderCmd) {
        match cmd {
            LoaderCmd::Allocate {
                file,
                alignment,
                zone,
            } => {
                self.buffer.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
                Self::append_fixed_string(&mut self.buffer, file);
                self.buffer.extend_from_slice(&alignment.to_le_bytes());
                self.buffer.push(zone);
                self.buffer.resize(self.buffer.len() + 63, 0); // Padding
            }
            LoaderCmd::AddPtr {
                pointer_file,
                pointee_file,
                pointer_offset,
                pointer_size,
            } => {
                self.buffer.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);
                Self::append_fixed_string(&mut self.buffer, pointer_file);
                Self::append_fixed_string(&mut self.buffer, pointee_file);
                self.buffer.extend_from_slice(&pointer_offset.to_le_bytes());
                self.buffer.push(pointer_size);
                self.buffer.resize(self.buffer.len() + 7, 0); // Padding
            }
            LoaderCmd::AddChecksum {
                file,
                result_offset,
                start,
                length,
            } => {
                self.buffer.extend_from_slice(&[0x03, 0x00, 0x00, 0x00]);
                Self::append_fixed_string(&mut self.buffer, file);
                self.buffer.extend_from_slice(&result_offset.to_le_bytes());
                self.buffer.extend_from_slice(&start.to_le_bytes());
                self.buffer.extend_from_slice(&length.to_le_bytes());
                self.buffer.resize(self.buffer.len() + 56, 0); // Padding
            }
        }
    }
}

/// Searches for an ACPI table with the given signature and returns its offset,
/// checksum offset, and length.
fn find_acpi_table(tables: &[u8], signature: &str) -> Result<(u32, u32, u32)> {
    if signature.len() != 4 {
        bail!("Signature must be 4 characters long, but got '{signature}'");
    }

    let sig_bytes = signature.as_bytes();

    let mut offset = 0;
    while offset < tables.len() {
        // Ensure there's enough space for a table header
        if offset + 8 > tables.len() {
            bail!("Table not found: {signature}");
        }

        let tbl_sig = &tables[offset..offset + 4];
        let tbl_len_bytes: [u8; 4] = tables[offset + 4..offset + 8].try_into().unwrap();
        let tbl_len = u32::from_le_bytes(tbl_len_bytes) as usize;

        if tbl_sig == sig_bytes {
            // Found the table
            return Ok((offset as u32, (offset + 9) as u32, tbl_len as u32));
        }

        if tbl_len == 0 {
            // Invalid table length, stop searching
            bail!("Found table with zero length at offset {offset}");
        }
        // Move to the next table
        offset += tbl_len;
    }

    bail!("Table not found: {signature}");
}
