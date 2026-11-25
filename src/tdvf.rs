/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * Copyright (c) 2025 Intel Corporation
 * SPDX-License-Identifier: Apache-2.0
 */
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha384};

use crate::num::read_le;
use crate::util::{debug_print_log, measure_log, measure_sha384, read_file_data, utf16_encode};
use crate::{Machine, OVMF_BOOT_EVENTS_VERSION, OVMF_SMBIOS_VERSION};

const PAGE_SIZE: u64 = 0x1000;
const MR_EXTEND_GRANULARITY: usize = 0x100;

/// QEMU fw_cfg BootMenu value: little-endian UINT16 = 0 (boot menu disabled).
const FW_CFG_BOOT_MENU_DISABLED: [u8; 2] = [0x00, 0x00];
/// QEMU fw_cfg bootorder content for direct boot via linuxboot_dma.
const FW_CFG_BOOTORDER: &[u8] = b"/rom@genroms/linuxboot_dma.bin\0";

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
fn measure_tdx_efi_variable(
    vendor_guid: &str,
    var_name: &str,
    var_data: Option<&[u8]>,
) -> Result<Vec<u8>> {
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

/// OVMF releases before upstream 2025.05 register one boot entry, while 2025.05 and later register two.
fn ovmf_exposes_single_boot_entry(ovmf_version: Option<(u16, u8)>) -> bool {
    !matches!(ovmf_version, Some(v) if v >= OVMF_BOOT_EVENTS_VERSION)
}

/// OVMF releases from upstream 2025.05 onward measure the fw_cfg BootMenu and bootorder items into RTMR0.
fn ovmf_exposes_fw_cfg_measurement(ovmf_version: Option<(u16, u8)>) -> bool {
    matches!(ovmf_version, Some(version) if version >= crate::OVMF_FW_CFG_VERSION)
}

/// OVMF releases from upstream 2025.05 onward measure the SMBIOS table into RTMR0.
pub fn ovmf_exposes_smbios_measurement(ovmf_version: Option<(u16, u8)>) -> bool {
    matches!(ovmf_version, Some(version) if version >= OVMF_SMBIOS_VERSION)
}

/// Returns the RTMR0 digest of the filtered SMBIOS table, or `None` if this OVMF version doesn't measure it.
/// Errors if the OVMF version requires it but no `smbios` path was provided in metadata.json.
fn measure_smbios(machine: &Machine) -> Result<Option<Vec<u8>>> {
    if !ovmf_exposes_smbios_measurement(machine.ovmf_version) {
        return Ok(None);
    }

    match machine.smbios {
        Some(smbios_path) => {
            let raw = read_file_data(smbios_path)?;
            // SmbiosMeasurementDxe blanks volatile fields before hashing; reproduce that filter to match the TD event log digest.
            let filtered = crate::smbios::filter_smbios_table(&raw);
            Ok(Some(measure_sha384(&filtered)))
        }
        None => {
            let (major, minor) = machine.ovmf_version.unwrap_or_default();
            bail!(
                "The used OVMF version {major}.{minor:02} measures SMBIOS but path to smbios binary was not provided in metadata.json");
        }
    }
}

/// Returns the BootOrder variable bytes OVMF creates for direct boot.
pub fn direct_boot_boot_order_bytes(ovmf_version: Option<(u16, u8)>) -> Vec<u8> {
    if ovmf_exposes_single_boot_entry(ovmf_version) {
        vec![0x00, 0x00]
    } else {
        vec![0x00, 0x00, 0x01, 0x00]
    }
}

/// Returns the Boot#### variable payloads (in BootOrder sequence) OVMF creates for direct boot.
pub fn direct_boot_boot_variable_payloads(ovmf_version: Option<(u16, u8)>) -> Result<Vec<Vec<u8>>> {
    if ovmf_exposes_single_boot_entry(ovmf_version) {
        // OVMF 2025.02: single Boot0000 entry (UiApp).
        let boot0000_hex = "090100002c0055006900410070007000000004071400c9bdb87cebf8344faaea3ee4af6516a10406140021aa2c4614760345836e8ab6f46623317fff0400";
        let boot0000 = hex::decode(boot0000_hex).context("Failed to decode boot0000 hex string")?;
        Ok(vec![boot0000])
    } else {
        // OVMF releases based on upstream 2025.05: Boot0000 (BootManagerMenuApp) and Boot0001 (EFI Firmware Setup).
        let boot0000_hex = "090100002c0042006f006f0074004d0061006e0061006700650072004d0065006e007500410070007000000004071400c9bdb87cebf8344faaea3ee4af6516a104061400dc5bc2eef267954db1d5f81b2039d11d7fff0400";
        let boot0000 = hex::decode(boot0000_hex).context("Failed to decode boot0000 hex string")?;

        let boot0001_hex = "010100002c0045004600490020004600690072006d007700610072006500200053006500740075007000000004071400c9bdb87cebf8344faaea3ee4af6516a10406140021aa2c4614760345836e8ab6f46623317fff0400";
        let boot0001 = hex::decode(boot0001_hex).context("Failed to decode boot0001 hex string")?;

        Ok(vec![boot0000, boot0001])
    }
}

/// Returns (boot_order_bytes, boot_entry_numbers) for the given boot mode.
/// For direct boot, returns simulated BootOrder bytes (OVMF-version-dependent) and an empty entry list.
/// For indirect boot, reads the boot order file and parses the UINT16 entry numbers.
fn parse_boot_order(machine: &Machine) -> Result<(Vec<u8>, Vec<u16>)> {
    if machine.direct_boot {
        // Direct boot: simulated boot order data
        let ovmf_version = machine.ovmf_version.ok_or_else(|| {
            anyhow!("OVMF version is required for direct boot boot-order measurement")
        })?;
        Ok((direct_boot_boot_order_bytes(Some(ovmf_version)), Vec::new()))
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
    let filename = format!("{}/Boot{:04X}.bin", machine.path_boot_xxxx, boot_entry_num);
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

        // Compute RTMR0 log
        let mut rtmr0_log = vec![td_hob_hash, cfv_hash];

        if ovmf_exposes_fw_cfg_measurement(machine.ovmf_version) {
            rtmr0_log.push(measure_sha384(&FW_CFG_BOOT_MENU_DISABLED));

            if machine.direct_boot {
                rtmr0_log.push(measure_sha384(FW_CFG_BOOTORDER));
            }
        }

        rtmr0_log.append(&mut vec![
            measure_tdx_efi_variable("8BE4DF61-93CA-11D2-AA0D-00E098032B8C", "SecureBoot", None)?,
            measure_tdx_efi_variable("8BE4DF61-93CA-11D2-AA0D-00E098032B8C", "PK", None)?,
            measure_tdx_efi_variable("8BE4DF61-93CA-11D2-AA0D-00E098032B8C", "KEK", None)?,
            measure_tdx_efi_variable("D719B2CB-3D3A-4596-A3BC-DAD00E67656F", "db", None)?,
            measure_tdx_efi_variable("D719B2CB-3D3A-4596-A3BC-DAD00E67656F", "dbx", None)?,
            measure_sha384(&[0x00, 0x00, 0x00, 0x00]), // Separator
            acpi_loader_hash,
            acpi_rsdp_hash,
            acpi_tables_hash,
        ]);

        if let Some(smbios_hash) = measure_smbios(machine)? {
            rtmr0_log.push(smbios_hash);
        }

        // Load boot order data and entries
        let (boot_order_data, boot_entries) = parse_boot_order(machine)?;

        rtmr0_log.push(measure_sha384(&boot_order_data));

        if machine.direct_boot {
            for boot_data in direct_boot_boot_variable_payloads(machine.ovmf_version)? {
                rtmr0_log.push(measure_sha384(&boot_data));
            }
        } else {
            for boot_entry_num in boot_entries {
                if let Some(boot_data) = load_boot_variable_if_exists(boot_entry_num, machine)? {
                    rtmr0_log.push(measure_sha384(&boot_data));
                }
            }
        }

        // Add SbatLevel if not direct boot
        if !machine.direct_boot {
            rtmr0_log.push(measure_tdx_efi_variable(
                "605DAB50-E046-4300-ABB6-3DD810DD8B23",
                "SbatLevel",
                Some(b"sbat,1,2021030218\n"),
            )?);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_machine(ovmf_version: Option<&str>) -> Machine<'static> {
        Machine::builder()
            .cpu_count(4)
            .memory_size(0x80000000)
            .firmware("")
            .kernel_cmdline("")
            .acpi_tables("")
            .rsdp("")
            .table_loader("")
            .boot_order("")
            .path_boot_xxxx("")
            .direct_boot(true)
            .metadata_path(std::path::Path::new(""))
            .create_acpi_table(false)
            .distribution("")
            .maybe_ovmf_version(ovmf_version.and_then(crate::parse_ovmf_version))
            .build()
    }

    #[test]
    fn parse_boot_order_ovmf_2025_02_returns_single_entry() {
        for ver in ["2025.02-3ubuntu2.2", "2025.02-3ubuntu2"] {
            let m = dummy_machine(Some(ver));
            let (data, entries) = parse_boot_order(&m).unwrap();
            assert_eq!(
                data,
                vec![0x00, 0x00],
                "OVMF {ver} should have single-entry BootOrder"
            );
            assert!(entries.is_empty());
            assert!(ovmf_exposes_single_boot_entry(crate::parse_ovmf_version(
                ver
            )));
        }
    }

    #[test]
    fn parse_boot_order_ovmf_2025_05_and_later_returns_two_entries() {
        for version in ["edk2-stable202505", "2025.11-3ubuntu7"] {
            let m = dummy_machine(Some(version));
            let (data, entries) = parse_boot_order(&m).unwrap();
            assert_eq!(
                data,
                vec![0x00, 0x00, 0x01, 0x00],
                "OVMF {version} should have two-entry BootOrder"
            );
            assert!(entries.is_empty());
            assert!(!ovmf_exposes_single_boot_entry(crate::parse_ovmf_version(
                version
            )));
        }
        assert!(ovmf_exposes_single_boot_entry(None));
    }

    #[test]
    fn parse_boot_order_none_ovmf_version_errors_for_direct_boot() {
        let m = dummy_machine(None);
        let err = parse_boot_order(&m).unwrap_err();
        assert!(
            err.to_string().contains("OVMF version is required"),
            "expected OVMF-version-required error, got: {err}",
        );
    }

    #[test]
    fn direct_boot_boot_variables_match_ovmf_version() {
        let boot0000_old = hex::decode(
            "090100002c0055006900410070007000000004071400c9bdb87cebf8344faaea3ee4af6516a10406140021aa2c4614760345836e8ab6f46623317fff0400"
        ).unwrap();

        let boot0000_new = hex::decode(
            "090100002c0042006f006f0074004d0061006e0061006700650072004d0065006e007500410070007000000004071400c9bdb87cebf8344faaea3ee4af6516a104061400dc5bc2eef267954db1d5f81b2039d11d7fff0400"
        ).unwrap();
        let boot0001_new = hex::decode(
            "010100002c0045004600490020004600690072006d007700610072006500200053006500740075007000000004071400c9bdb87cebf8344faaea3ee4af6516a10406140021aa2c4614760345836e8ab6f46623317fff0400"
        ).unwrap();

        let expected = vec![
            (Some("2025.02-3ubuntu2.2"), vec![boot0000_old.clone()]),
            (
                Some("2025.11-3ubuntu7"),
                vec![boot0000_new.clone(), boot0001_new.clone()],
            ),
            (None, vec![boot0000_old]),
        ];

        for (version, expected_payloads) in expected {
            let actual =
                direct_boot_boot_variable_payloads(version.and_then(crate::parse_ovmf_version))
                    .unwrap();
            assert_eq!(
                actual, expected_payloads,
                "OVMF version mismatch for {version:?}"
            );
        }
    }

    // The threshold is (2025, 5); 2025.02 must not trigger fw_cfg measurement.
    #[test]
    fn fw_cfg_version_boundary_is_exactly_2025_05() {
        assert!(!ovmf_exposes_fw_cfg_measurement(crate::parse_ovmf_version(
            "edk2-stable202502"
        )));
        assert!(ovmf_exposes_fw_cfg_measurement(crate::parse_ovmf_version(
            "edk2-stable202505"
        )));
        assert!(ovmf_exposes_fw_cfg_measurement(crate::parse_ovmf_version(
            "2025.11-3ubuntu7"
        )));
        assert!(!ovmf_exposes_fw_cfg_measurement(crate::parse_ovmf_version(
            ""
        )));
    }

    // The threshold is (2025, 05); 2025.02 must not trigger SMBIOS measurement.
    #[test]
    fn smbios_version_boundary_is_exactly_2025_05() {
        assert!(!ovmf_exposes_smbios_measurement(crate::parse_ovmf_version(
            "edk2-stable202502"
        )));
        assert!(ovmf_exposes_smbios_measurement(crate::parse_ovmf_version(
            "edk2-stable202505"
        )));
        assert!(ovmf_exposes_smbios_measurement(crate::parse_ovmf_version(
            "2025.11-3ubuntu7"
        )));
        assert!(!ovmf_exposes_smbios_measurement(None));
    }

    #[test]
    fn measure_smbios_returns_none_when_ovmf_predates_measurement() {
        let m = dummy_machine(Some("edk2-stable202502"));
        assert!(measure_smbios(&m).unwrap().is_none());
    }

    #[test]
    fn measure_smbios_errors_when_required_but_missing() {
        let m = dummy_machine(Some("edk2-stable202505"));
        let err = measure_smbios(&m).unwrap_err();
        assert!(
            err.to_string()
                .contains("path to smbios binary was not provided in metadata.json"),
            "unexpected error: {err}"
        );
    }

    // A configured but unreadable smbios path must propagate as an Error.
    #[test]
    fn measure_smbios_propagates_read_error_for_missing_file() {
        let m = Machine::builder()
            .cpu_count(4)
            .memory_size(0x80000000)
            .firmware("")
            .kernel_cmdline("")
            .acpi_tables("")
            .rsdp("")
            .table_loader("")
            .boot_order("")
            .path_boot_xxxx("")
            .maybe_smbios(Some("/nonexistent/path/smbios.bin"))
            .direct_boot(true)
            .metadata_path(std::path::Path::new(""))
            .create_acpi_table(false)
            .distribution("")
            .maybe_ovmf_version(crate::parse_ovmf_version("edk2-stable202505"))
            .build();

        let err = measure_smbios(&m).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to load file /nonexistent/path/smbios.bin"),
            "unexpected error: {err}"
        );
    }
}
