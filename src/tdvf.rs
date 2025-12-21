/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * SPDX-License-Identifier: Apache-2.0
 */
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha384};

use crate::num::read_le;
use crate::util::{debug_print_log, measure_sha384, measure_log, utf16_encode, read_file_data};
use crate::Machine;

const PAGE_SIZE: u64 = 0x1000;
const MR_EXTEND_GRANULARITY: usize = 0x100;

const ATTRIBUTE_MR_EXTEND: u32 = 0x00000001;
const ATTRIBUTE_PAGE_AUG: u32 = 0x00000002;

const TDVF_SECTION_TD_CFV: u32 = 0x01;
const TDVF_SECTION_TD_HOB: u32 = 0x02;
const TDVF_SECTION_TEMP_MEM: u32 = 0x03;

#[derive(Debug)]
struct TdvfSection {
    data_offset: u32,
    raw_data_size: u32,
    memory_address: u64,
    memory_data_size: u64,
    sec_type: u32,
    attributes: u32,
}

#[derive(Debug)]
pub(crate) struct Tdvf<'a> {
    fw: &'a [u8],
    sections: Vec<TdvfSection>,
}

/// Encodes a GUID string into its binary representation.
fn encode_guid(guid_str: &str) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(16);
    let atoms: Vec<&str> = guid_str.split('-').collect();

    if atoms.len() != 5 {
        return Err(anyhow!("Invalid GUID format"));
    }

    for (idx, atom) in atoms.iter().enumerate() {
        let raw = hex::decode(atom).context("Failed to decode hex in GUID")?;

        if idx <= 2 {
            // Little-endian: reverse the bytes
            for i in (0..raw.len()).rev() {
                data.push(raw[i]);
            }
        } else {
            // Big-endian: keep as-is
            data.extend_from_slice(&raw);
        }
    }

    Ok(data)
}

/// Measures an EFI variable event.
fn measure_tdx_efi_variable(vendor_guid: &str, var_name: &str, var_data: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    data.extend_from_slice(&encode_guid(vendor_guid)?);
    data.extend_from_slice(&(var_name.len() as u64).to_le_bytes());

    // Include variable data size
    let data_size = var_data.map_or(0, |d| d.len()) as u64;
    data.extend_from_slice(&data_size.to_le_bytes());

    // Add variable name (UTF-16 encoded)
    data.extend(utf16_encode(var_name));

    // Add variable data if present
    if let Some(var_data) = var_data {
        data.extend_from_slice(var_data);
    }

    Ok(measure_sha384(&data))
}

/// Loads boot order data and parses boot entry numbers based on boot mode.
/// For direct boot, returns a simulated boot order with no boot entries.
/// For indirect boot, reads the boot order file and extracts the list of boot entry numbers.
fn parse_boot_order(machine: &Machine) -> Result<(Vec<u8>, Vec<u16>)> {
    if machine.direct_boot {
        // Direct boot: simulated boot order data
        Ok((vec![0, 0], Vec::new()))
    } else if !machine.boot_order.is_empty() {
        // Indirect boot: read and parse the boot order file
        let boot_order_data = read_file_data(machine.boot_order)?;

        // Validate and parse boot order data
        if boot_order_data.len() % 2 != 0 {
            bail!("BootOrder data length must be even (array of UINT16s)");
        }

        let mut boot_entries = Vec::new();
        for chunk in boot_order_data.chunks(2) {
            let entry_num = u16::from_le_bytes([chunk[0], chunk[1]]);
            boot_entries.push(entry_num);
        }

        Ok((boot_order_data, boot_entries))
    } else {
        Err(anyhow!("Boot order file is required for indirect boot"))
    }
}

/// Loads boot variable data if the corresponding file exists
fn load_boot_variable_if_exists(boot_entry_num: u16, machine: &Machine) -> Result<Option<Vec<u8>>> {
    let filename = format!("{}Boot{:04}.bin", machine.path_boot_xxxx, boot_entry_num);
    // Try to read the file, return None if it doesn't exist
    match read_file_data(&filename) {
        Ok(data) => Ok(Some(data)),
        Err(_) => {
            // File doesn't exist or can't be read, skip this boot entry
            Ok(None)
        }
    }
}

impl<'a> Tdvf<'a> {
    pub fn parse(fw: &'a [u8]) -> Result<Tdvf<'a>> {
        const TDX_METADATA_OFFSET_GUID: &str = "e47a6535-984a-4798-865e-4685a7bf8ec2";
        const TABLE_FOOTER_GUID: &str = "96b582de-1fb2-45f7-baea-a366c55a082d";
        const BYTES_AFTER_TABLE_FOOTER: usize = 32;

        let offset = fw.len() - BYTES_AFTER_TABLE_FOOTER;
        let encoded_footer_guid = encode_guid(TABLE_FOOTER_GUID)?;
        let guid = &fw[offset - 16..offset];

        if guid != encoded_footer_guid {
            bail!("Failed to parse TDVF metadata: Invalid footer GUID");
        }

        let tables_len =
            u16::from_le_bytes(fw[offset - 18..offset - 16].try_into().unwrap()) as usize;
        if tables_len == 0 || tables_len > offset - 18 {
            bail!("Failed to parse TDVF metadata: Invalid tables length");
        }
        let tables = &fw[offset - 18 - tables_len..offset - 18];
        let mut offset = tables.len();

        let mut data: Option<&[u8]> = None;
        let encoded_guid = encode_guid(TDX_METADATA_OFFSET_GUID)?;
        loop {
            if offset < 18 {
                break;
            }
            let guid = &tables[offset - 16..offset];
            let entry_len = read_le::<u16>(tables, offset - 18, "entry length")? as usize;
            if entry_len > offset - 18 {
                bail!("Failed to parse TDVF metadata: Invalid entry length");
            }
            if guid == encoded_guid {
                data = Some(&tables[offset - 18 - entry_len..offset - 18]);
                break;
            }
            offset -= entry_len;
        }

        let data = data.context("Failed to parse TDVF metadata: Missing TDVF metadata")?;

        let tdvf_meta_offset =
            u32::from_le_bytes(data[data.len() - 4..].try_into().unwrap()) as usize;
        let tdvf_meta_offset = fw.len() - tdvf_meta_offset;
        let tdvf_meta_desc = &fw[tdvf_meta_offset..tdvf_meta_offset + 16];

        if &tdvf_meta_desc[..4] != b"TDVF" {
            bail!("Failed to parse TDVF metadata: Invalid TDVF descriptor");
        }
        let tdvf_version = u32::from_le_bytes(tdvf_meta_desc[8..12].try_into().unwrap());
        if tdvf_version != 1 {
            bail!("Failed to parse TDVF metadata: Unsupported TDVF version");
        }
        let num_sections = u32::from_le_bytes(tdvf_meta_desc[12..16].try_into().unwrap()) as usize;

        let mut meta = Tdvf {
            fw,
            sections: Vec::new(),
        };
        for i in 0..num_sections {
            let sec_offset = tdvf_meta_offset + 16 + 32 * i;
            let sec_data = &fw[sec_offset..sec_offset + 32];
            let s = TdvfSection {
                data_offset: u32::from_le_bytes(sec_data[0..4].try_into().unwrap()),
                raw_data_size: u32::from_le_bytes(sec_data[4..8].try_into().unwrap()),
                memory_address: u64::from_le_bytes(sec_data[8..16].try_into().unwrap()),
                memory_data_size: u64::from_le_bytes(sec_data[16..24].try_into().unwrap()),
                sec_type: u32::from_le_bytes(sec_data[24..28].try_into().unwrap()),
                attributes: u32::from_le_bytes(sec_data[28..32].try_into().unwrap()),
            };

            if s.memory_address % PAGE_SIZE != 0 {
                bail!("Failed to parse TDVF metadata: Section memory address not aligned");
            }
            if s.memory_data_size < s.raw_data_size as u64 {
                bail!("Failed to parse TDVF metadata: Section memory data size less than raw");
            }
            if s.memory_data_size % PAGE_SIZE != 0 {
                bail!("Failed to parse TDVF metadata: Section memory data size not aligned");
            }
            if s.attributes & ATTRIBUTE_MR_EXTEND != 0
                && s.raw_data_size as u64 > s.memory_data_size
            {
                bail!("Failed to parse TDVF metadata: Section raw data size less than memory");
            }

            meta.sections.push(s);
        }

        Ok(meta)
    }

    pub fn mrtd(&self) -> Result<Vec<u8>> {
        let mut h = Sha384::new();

        let mem_page_add = |h: &mut Sha384, s: &TdvfSection, page: u64| {
            if s.attributes & ATTRIBUTE_PAGE_AUG == 0 {
                let mut buf = [0u8; 128];
                buf[..12].copy_from_slice(b"MEM.PAGE.ADD");
                let gpa = s.memory_address + page * PAGE_SIZE;
                buf[16..24].copy_from_slice(&gpa.to_le_bytes());
                h.update(buf);
            }
        };

        let mr_extend = |h: &mut Sha384, s: &TdvfSection, page: u64| {
            if s.attributes & ATTRIBUTE_MR_EXTEND != 0 {
                for i in 0..(PAGE_SIZE as usize / MR_EXTEND_GRANULARITY) {
                    let mut buf = [0u8; 128];
                    buf[..9].copy_from_slice(b"MR.EXTEND");
                    let gpa =
                        s.memory_address + page * PAGE_SIZE + (i * MR_EXTEND_GRANULARITY) as u64;
                    buf[16..24].copy_from_slice(&gpa.to_le_bytes());
                    h.update(buf);

                    let chunk_offset = s.data_offset as usize
                        + (page * PAGE_SIZE) as usize
                        + i * MR_EXTEND_GRANULARITY;
                    h.update(&self.fw[chunk_offset..chunk_offset + MR_EXTEND_GRANULARITY]);
                }
            }
        };

        for s in &self.sections {
            let num_pages = s.memory_data_size / PAGE_SIZE;

            for page in 0..num_pages {
                mem_page_add(&mut h, s, page);
                mr_extend(&mut h, s, page);
            }
        }
        Ok(h.finalize().to_vec())
    }

    pub fn rtmr0(&self, machine: &Machine) -> Result<Vec<u8>> {
        // Calculate measurement of the TD Hand-Off Block (TD-HOB)
        let td_hob_hash = self.measure_td_hob(machine.memory_size)?;

        // Calculate measurement of the Configuration Firmware Volume (CFV)
        let cfv_hash = self.measure_cfv().context("Failed to find CFV section")?;

        // Build ACPI tables
        let tables = machine.build_tables()?;
        let acpi_tables_hash = measure_sha384(&tables.tables);
        let acpi_rsdp_hash = measure_sha384(&tables.rsdp);
        let acpi_loader_hash = measure_sha384(&tables.loader);

        // Load boot order data and entries
        let (boot_order_data, boot_entries) = parse_boot_order(machine)?;

        // Compute RTMR0 log
        let mut rtmr0_log = vec![
            td_hob_hash,
            cfv_hash,
            measure_tdx_efi_variable("8BE4DF61-93CA-11D2-AA0D-00E098032B8C", "SecureBoot", None)?,
            measure_tdx_efi_variable("8BE4DF61-93CA-11D2-AA0D-00E098032B8C", "PK", None)?,
            measure_tdx_efi_variable("8BE4DF61-93CA-11D2-AA0D-00E098032B8C", "KEK", None)?,
            measure_tdx_efi_variable("D719B2CB-3D3A-4596-A3BC-DAD00E67656F", "db", None)?,
            measure_tdx_efi_variable("D719B2CB-3D3A-4596-A3BC-DAD00E67656F", "dbx", None)?,
            measure_sha384(&[0x00, 0x00, 0x00, 0x00]), // Separator
            acpi_loader_hash,
            acpi_rsdp_hash,
            acpi_tables_hash,
            measure_sha384(&boot_order_data), // Always measure BootOrder itself
        ];

        if machine.direct_boot {
            // Boot0000 data for direct boot mode
            let boot0000_hex = "090100002c0055006900410070007000000004071400c9bdb87cebf8344faaea3ee4af6516a10406140021aa2c4614760345836e8ab6f46623317fff0400";
            let boot0000 = hex::decode(boot0000_hex).context("Failed to decode boot0000 hex string")?;
            rtmr0_log.push(measure_sha384(&boot0000));
        } else {
            for boot_entry_num in boot_entries {
                if let Some(boot_data) = load_boot_variable_if_exists(boot_entry_num, machine)? {
                    rtmr0_log.push(measure_sha384(&boot_data));
                }
            }
        }

        // Add SbatLevel if not direct boot
        if !machine.direct_boot {
            rtmr0_log.push(measure_tdx_efi_variable("605DAB50-E046-4300-ABB6-3DD810DD8B23", "SbatLevel", Some(b"sbat,1,2021030218\n"))?);
        }

        debug_print_log("RTMR0", &rtmr0_log);
        Ok(measure_log(&rtmr0_log))
    }

    fn measure_td_hob(&self, memory_size: u64) -> Result<Vec<u8>> {
        let mut memory_acceptor = MemoryAcceptor::new(0, memory_size);
        let mut td_hob = Vec::new();

        let mut td_hob_base_addr = 0x809000u64;
        for s in &self.sections {
            if let TDVF_SECTION_TD_HOB | TDVF_SECTION_TEMP_MEM = s.sec_type {
                memory_acceptor.accept(s.memory_address, s.memory_address + s.memory_data_size);
            }
            if s.sec_type == TDVF_SECTION_TD_HOB {
                td_hob_base_addr = s.memory_address;
            }
        }

        td_hob.extend_from_slice(&[0x01, 0x00]); // HobType
        td_hob.extend_from_slice(&56u16.to_le_bytes()); // HobLength
        td_hob.extend_from_slice(&[0u8; 4]); // Reserved
        td_hob.extend_from_slice(&9u32.to_le_bytes()); // Version
        td_hob.extend_from_slice(&[0u8; 4]); // BootMode
        td_hob.extend_from_slice(&[0u8; 8]); // EfiMemoryTop
        td_hob.extend_from_slice(&[0u8; 8]); // EfiMemoryBottom
        td_hob.extend_from_slice(&[0u8; 8]); // EfiFreeMemoryTop
        td_hob.extend_from_slice(&[0u8; 8]); // EfiFreeMemoryBottom
        td_hob.extend_from_slice(&[0u8; 8]); // EfiEndOfHobList (placeholder)

        let mut add_memory_resource_hob = |resource_type: u8, start: u64, length: u64| {
            td_hob.extend_from_slice(&[0x03, 0x00]); // HobType
            td_hob.extend_from_slice(&48u16.to_le_bytes()); // HobLength
            td_hob.extend_from_slice(&[0u8; 4]); // Reserved
            td_hob.extend_from_slice(&[0u8; 16]); // Owner
            td_hob.extend_from_slice(&resource_type.to_le_bytes());
            td_hob.extend_from_slice(&[0u8; 3]); // Padding for resource type
            td_hob.extend_from_slice(&7u32.to_le_bytes()); // ResourceAttribute
            td_hob.extend_from_slice(&start.to_le_bytes());
            td_hob.extend_from_slice(&length.to_le_bytes());
        };

        let (_, last_start, last_end) = memory_acceptor.ranges.pop().expect("No ranges");

        for (accepted, start, end) in memory_acceptor.ranges {
            if accepted {
                add_memory_resource_hob(0x00, start, end - start);
            } else {
                add_memory_resource_hob(0x07, start, end - start);
            }
        }

        if memory_size >= 0xB0000000 {
            add_memory_resource_hob(0x07, last_start, 0x80000000u64 - last_start);
            add_memory_resource_hob(0x07, 0x100000000, last_end - 0x80000000u64);
        } else {
            add_memory_resource_hob(0x07, last_start, last_end - last_start);
        }

        let end_of_hob_list = td_hob_base_addr + td_hob.len() as u64 + 8;
        td_hob[48..56].copy_from_slice(&end_of_hob_list.to_le_bytes());

        Ok(measure_sha384(&td_hob))
    }

    fn measure_cfv(&self) -> Result<Vec<u8>> {
        for section in &self.sections {
            if section.sec_type == TDVF_SECTION_TD_CFV {
                let start = section.data_offset as usize;
                let end = start + section.raw_data_size as usize;

                if end > self.fw.len() {
                    return Err(anyhow!("CFV section extends beyond firmware data."));
                }

                return Ok(measure_sha384(&self.fw[start..end]));
            }
        }

        Err(anyhow!("CFV section does not exist."))
    }
}

struct MemoryAcceptor {
    ranges: Vec<(bool, u64, u64)>,
}

impl MemoryAcceptor {
    fn new(start: u64, size: u64) -> Self {
        Self {
            ranges: vec![(false, start, start + size)],
        }
    }

    fn accept(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }

        let mut new_ranges = Vec::new();

        for &(is_accepted, range_start, range_end) in &self.ranges {
            if is_accepted || range_end <= start || range_start >= end {
                new_ranges.push((is_accepted, range_start, range_end));
            } else {
                if range_start < start {
                    new_ranges.push((false, range_start, start));
                }
                if range_end > end {
                    new_ranges.push((false, end, range_end));
                }
            }
        }
        new_ranges.push((true, start, end));
        new_ranges.sort_by_key(|&(_, start, _)| start);
        self.ranges = new_ranges;
    }
}
