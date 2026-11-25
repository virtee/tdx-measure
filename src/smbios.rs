/*
 * Copyright (c) 2026 Intel Corporation
 * SPDX-License-Identifier: Apache-2.0
 */

// ---------------------------------------------------------------------------
// SMBIOS filtering - byte-for-byte port of EDK II's MdeModulePkg/Universal/SmbiosMeasurementDxe/SmbiosMeasurementDxe.c
// ---------------------------------------------------------------------------

const FLAG_IS_STRING: u8 = 1;
const SMBIOS_HEADER_SIZE: usize = 4; // Type(1) + Length(1) + Handle(2)

struct FilterRule {
    offset: usize,
    size: usize,
    flags: u8,
}

static TYPE_01: &[FilterRule] = &[
    FilterRule {
        offset: 0x07,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // SerialNumber
    FilterRule {
        offset: 0x08,
        size: 16,
        flags: 0,
    }, // UUID
    FilterRule {
        offset: 0x18,
        size: 1,
        flags: 0,
    }, // WakeUpType
];
static TYPE_02: &[FilterRule] = &[
    FilterRule {
        offset: 0x07,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // SerialNumber
    FilterRule {
        offset: 0x0A,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // LocationInChassis
];
static TYPE_03: &[FilterRule] = &[
    FilterRule {
        offset: 0x07,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // SerialNumber
    FilterRule {
        offset: 0x08,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // AssetTag
];
static TYPE_04: &[FilterRule] = &[
    FilterRule {
        offset: 0x20,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // SerialNumber
    FilterRule {
        offset: 0x21,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // AssetTag
    FilterRule {
        offset: 0x22,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // PartNumber
    FilterRule {
        offset: 0x23,
        size: 1,
        flags: 0,
    }, // CoreCount
    FilterRule {
        offset: 0x24,
        size: 1,
        flags: 0,
    }, // EnabledCoreCount
    FilterRule {
        offset: 0x25,
        size: 1,
        flags: 0,
    }, // ThreadCount
    FilterRule {
        offset: 0x2A,
        size: 2,
        flags: 0,
    }, // CoreCount2
    FilterRule {
        offset: 0x2C,
        size: 2,
        flags: 0,
    }, // EnabledCoreCount2
    FilterRule {
        offset: 0x2E,
        size: 2,
        flags: 0,
    }, // ThreadCount2
    FilterRule {
        offset: 0x11,
        size: 1,
        flags: 0,
    }, // Voltage
    FilterRule {
        offset: 0x16,
        size: 2,
        flags: 0,
    }, // CurrentSpeed
];
static TYPE_11: &[FilterRule] = &[
    FilterRule {
        offset: 0x18,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // SerialNumber
    FilterRule {
        offset: 0x19,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // AssetTag
    FilterRule {
        offset: 0x1A,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // PartNumber
];
static TYPE_16: &[FilterRule] = &[
    FilterRule {
        offset: 0x07,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // SerialNumber
    FilterRule {
        offset: 0x10,
        size: 2,
        flags: 0,
    }, // SBDSSerialNumber
    FilterRule {
        offset: 0x12,
        size: 2,
        flags: 0,
    }, // SBDSManufactureDate
];
static TYPE_17: &[FilterRule] = &[
    FilterRule {
        offset: 0x05,
        size: 2,
        flags: 0,
    }, // ResetCount
];
static TYPE_1B: &[FilterRule] = &[
    FilterRule {
        offset: 0x0C,
        size: 2,
        flags: 0,
    }, // NominalSpeed
];
static TYPE_27: &[FilterRule] = &[
    FilterRule {
        offset: 0x08,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // SerialNumber
    FilterRule {
        offset: 0x09,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // AssetTagNumber
    FilterRule {
        offset: 0x0A,
        size: 1,
        flags: FLAG_IS_STRING,
    }, // ModelPartNumber
];

/// Returns the full size of the SMBIOS structure at offset `off`, or zero if it is invalid or truncated.
fn smbios_structure_size(buf: &[u8], off: usize) -> usize {
    // Check that the buffer has enough bytes for the 4-byte header, which includes the type (1 byte), length (1 byte), and handle (2 bytes) fields.
    if off + SMBIOS_HEADER_SIZE > buf.len() {
        return 0;
    }

    // Check that the formatted-area length, which includes the 4-byte header but excludes the trailing string set, is valid and fits within the buffer.
    let length = buf[off + 1] as usize;
    if length < SMBIOS_HEADER_SIZE || off + length > buf.len() {
        return 0;
    }

    // The optional string set ends at the first double-NUL terminator.
    let mut p = off + length;
    while p + 1 < buf.len() && !(buf[p] == 0 && buf[p + 1] == 0) {
        p += 1;
    }

    // A complete string set must include both bytes of the final double-NUL terminator.
    if p + 1 >= buf.len() {
        return 0;
    }

    // Include the formatted area, string set, and both terminator bytes in the structure size.
    (p + 2) - off
}

/// Return (begin, end) byte span of the `string_id`-th string (1-based) in the string set.
fn smbios_nth_string_span(
    buf: &[u8],
    start: usize,
    length: usize,
    string_id: u8,
) -> Option<(usize, usize)> {
    let mut p = start + length;
    let mut idx: u8 = 0;
    while p < buf.len() {
        if buf[p] == 0 {
            if p + 1 < buf.len() && buf[p + 1] == 0 {
                return None; // end of string set
            }
            p += 1;
            continue;
        }
        let begin = p;
        while p < buf.len() && buf[p] != 0 {
            p += 1;
        }
        // The buffer ended before this string was NUL-terminated: the string set is truncated.
        if p >= buf.len() {
            return None;
        }
        idx += 1;
        if idx == string_id {
            return Some((begin, p));
        }
    }
    None
}

/// Apply OVMF's FilterSmbiosEntry() to one structure, in place.
fn smbios_filter_entry(buf: &mut [u8], off: usize, size: usize) {
    let stype = buf[off];
    let slen = buf[off + 1] as usize;

    match stype {
        0x01 => smbios_filter_fields(buf, off, slen, TYPE_01),
        0x02 => smbios_filter_fields(buf, off, slen, TYPE_02),
        0x03 => smbios_filter_fields(buf, off, slen, TYPE_03),
        0x04 => smbios_filter_fields(buf, off, slen, TYPE_04),
        0x11 => smbios_filter_fields(buf, off, slen, TYPE_11),
        0x16 => smbios_filter_fields(buf, off, slen, TYPE_16),
        0x17 => smbios_filter_fields(buf, off, slen, TYPE_17),
        0x1B => smbios_filter_fields(buf, off, slen, TYPE_1B),
        0x27 => smbios_filter_fields(buf, off, slen, TYPE_27),
        // EDK2 zeros the bodies of OEM Strings, Event Log, 32-bit Memory Error, Boot Integrity Services, 64-bit Memory Error, and all OEM-defined types (128+).
        0x0B | 0x0F | 0x12 | 0x1F | 0x21 | 0x80..=u8::MAX => {
            for b in &mut buf[off + SMBIOS_HEADER_SIZE..off + size] {
                *b = 0;
            }
        }
        _ => {} // Pass through all other types without modification
    }
}

/// Applies field-specific SMBIOS filtering by blanking referenced strings and zeroing volatile fields.
fn smbios_filter_fields(buf: &mut [u8], off: usize, slen: usize, fields: &[FilterRule]) {
    for f in fields {
        // Ignore fields not present in this structure's formatted area.
        if slen < f.offset + f.size {
            continue;
        }

        // Blank any referenced string in the variable-length string set.
        if f.flags & FLAG_IS_STRING != 0 {
            // String fields contain a one-based index into the trailing string set.
            let string_id = buf[off + f.offset];
            if string_id != 0 {
                if let Some((sb, se)) = smbios_nth_string_span(buf, off, slen, string_id) {
                    // Preserve the string length while removing its variable content.
                    for b in &mut buf[sb..se] {
                        *b = b' ';
                    }
                }
            }
        }

        // Clear the field itself after blanking any referenced string.
        for b in &mut buf[off + f.offset..off + f.offset + f.size] {
            *b = 0;
        }
    }
}

/// Returns a copy of the SMBIOS structure table with OVMF volatile fields filtered according to OVMF's `FilterSmbiosTable()`.
/// Reference: <https://github.com/tianocore/edk2/blob/master/MdeModulePkg/Universal/SmbiosMeasurementDxe/SmbiosMeasurementDxe.c>.
/// An SMBIOS table is a sequence of structures terminated by an End-of-Table structure.
/// +--------------------+--------------------+-----+----------------------+
/// | Structure 0        | Structure 1        | ... | End-of-Table (127)   |
/// +--------------------+--------------------+-----+----------------------+
/// Each structure consists of a formatted area followed by a variable-length string set.
/// +--------+----------+----------------+--------------------------+-------------+
/// | Type   | Length   | Handle         | Formatted fields         | String set  |
/// | 1 byte | 1 byte   | 2 bytes (LE)   | variable by type         | variable    |
/// +--------+----------+----------------+--------------------------+-------------+
/// <------------------------- Length bytes ------------------------->
/// The string set contains NUL-terminated strings and ends with an additional NUL byte.
pub(crate) fn filter_smbios_table(data: &[u8]) -> Vec<u8> {
    // Iterate over the structures in the table, filtering each one in place.
    let mut buf = data.to_vec();
    let mut off = 0;
    // Determine the size of the current smbios structure.
    // A zero size denotes a truncated or malformed structure.
    while off < buf.len() {
        let size = smbios_structure_size(&buf, off);
        if size == 0 {
            break;
        }

        // Filter the structure in place, according to OVMF's FilterSmbiosEntry().
        smbios_filter_entry(&mut buf, off, size);

        // Check for End-of-Table structure (type 127) and stop processing if found.
        if buf[off] == 127 {
            // End-of-Table
            break;
        }
        off += size;
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append_smbios_entry(
        table: &mut Vec<u8>,
        expected: &mut Vec<u8>,
        stype: u8,
        volatile_fields: &[(usize, usize)],
        string_fields: &[usize],
        zero_body: bool,
    ) {
        const FORMATTED_LENGTH: usize = 0x30;

        let mut entry = vec![0xA5; FORMATTED_LENGTH];
        entry[0] = stype;
        entry[1] = FORMATTED_LENGTH as u8;
        entry[2..4].copy_from_slice(&0x1234u16.to_le_bytes());

        let mut string_spans = Vec::new();
        for (index, &field_offset) in string_fields.iter().enumerate() {
            entry[field_offset] = (index + 1) as u8;
            let start = entry.len();
            entry.extend_from_slice(format!("string{}", index + 1).as_bytes());
            string_spans.push((start, entry.len()));
            entry.push(0);
        }
        if string_fields.is_empty() {
            entry.extend_from_slice(&[0, 0]);
        } else {
            entry.push(0);
        }

        let mut filtered = entry.clone();
        if zero_body {
            filtered[SMBIOS_HEADER_SIZE..].fill(0);
        } else {
            for &(offset, size) in volatile_fields {
                filtered[offset..offset + size].fill(0);
            }
            for (start, end) in string_spans {
                filtered[start..end].fill(b' ');
            }
        }

        table.extend_from_slice(&entry);
        expected.extend_from_slice(&filtered);
    }

    #[test]
    fn filter_smbios_table_filters_all_defined_types() {
        let mut input = Vec::new();
        let mut expected = Vec::new();

        append_smbios_entry(
            &mut input,
            &mut expected,
            0x01,
            &[(0x07, 1), (0x08, 16), (0x18, 1)],
            &[0x07],
            false,
        );
        append_smbios_entry(
            &mut input,
            &mut expected,
            0x02,
            &[(0x07, 1), (0x0A, 1)],
            &[0x07, 0x0A],
            false,
        );
        append_smbios_entry(
            &mut input,
            &mut expected,
            0x03,
            &[(0x07, 1), (0x08, 1)],
            &[0x07, 0x08],
            false,
        );
        append_smbios_entry(
            &mut input,
            &mut expected,
            0x04,
            &[
                (0x11, 1),
                (0x16, 2),
                (0x20, 1),
                (0x21, 1),
                (0x22, 1),
                (0x23, 1),
                (0x24, 1),
                (0x25, 1),
                (0x2A, 2),
                (0x2C, 2),
                (0x2E, 2),
            ],
            &[0x20, 0x21, 0x22],
            false,
        );
        append_smbios_entry(&mut input, &mut expected, 0x0B, &[], &[], true);
        append_smbios_entry(&mut input, &mut expected, 0x0F, &[], &[], true);
        append_smbios_entry(
            &mut input,
            &mut expected,
            0x11,
            &[(0x18, 1), (0x19, 1), (0x1A, 1)],
            &[0x18, 0x19, 0x1A],
            false,
        );
        append_smbios_entry(&mut input, &mut expected, 0x12, &[], &[], true);
        append_smbios_entry(
            &mut input,
            &mut expected,
            0x16,
            &[(0x07, 1), (0x10, 2), (0x12, 2)],
            &[0x07],
            false,
        );
        append_smbios_entry(&mut input, &mut expected, 0x17, &[(0x05, 2)], &[], false);
        append_smbios_entry(&mut input, &mut expected, 0x1B, &[(0x0C, 2)], &[], false);
        append_smbios_entry(&mut input, &mut expected, 0x1F, &[], &[], true);
        append_smbios_entry(&mut input, &mut expected, 0x21, &[], &[], true);
        append_smbios_entry(
            &mut input,
            &mut expected,
            0x27,
            &[(0x08, 1), (0x09, 1), (0x0A, 1)],
            &[0x08, 0x09, 0x0A],
            false,
        );
        input.extend_from_slice(&[0x7F, 0x04, 0x00, 0x00, 0x00, 0x00]);
        expected.extend_from_slice(&[0x7F, 0x04, 0x00, 0x00, 0x00, 0x00]);

        assert_eq!(filter_smbios_table(&input), expected);
    }

    #[test]
    fn filter_smbios_table_passes_through_unknown_types() {
        // Type 0x00 (BIOS Information) has no filter rule — bytes must be unchanged.
        let mut buf = vec![
            0x00, 0x18, 0x00, 0x00, // type=0, length=24, handle=0
        ];
        buf.extend_from_slice(&[0xBB; 20]); // payload
        buf.extend_from_slice(b"\x00\x00"); // empty string set
        buf.extend_from_slice(&[0x7F, 0x04, 0x00, 0x00, 0x00, 0x00]); // End-of-Table
        let input = buf.clone();
        let filtered = filter_smbios_table(&input);
        assert_eq!(
            &filtered[4..24],
            &[0xBBu8; 20],
            "type-0 payload must be unchanged"
        );
    }

    #[test]
    fn filter_smbios_table_zeros_oem_type_body() {
        // OEM types (>= 128) must have everything after the 4-byte header zeroed.
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x80, 0x08, 0x00, 0x00]); // type=128, length=8
        buf.extend_from_slice(&[0xCC; 4]); // payload — must be zeroed
        buf.extend_from_slice(b"\x00\x00"); // empty string set
        buf.extend_from_slice(&[0x7F, 0x04, 0x00, 0x00, 0x00, 0x00]);
        let filtered = filter_smbios_table(&buf);
        assert_eq!(&filtered[4..8], &[0u8; 4], "OEM type body must be zeroed");
    }

    #[test]
    fn smbios_structure_size_rejects_truncated_header() {
        // Only 2 bytes available where 4 are required.
        let buf = vec![0x01, 0x1B];
        assert_eq!(smbios_structure_size(&buf, 0), 0);
    }

    #[test]
    fn smbios_structure_size_rejects_length_smaller_than_header() {
        // Declared length (3) is less than the mandatory 4-byte header.
        let buf = vec![0x01, 0x03, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(smbios_structure_size(&buf, 0), 0);
    }

    #[test]
    fn smbios_structure_size_rejects_truncated_formatted_area() {
        // Declared length (0x10) extends past the end of the buffer.
        let buf = vec![0x01, 0x10, 0x00, 0x00];
        assert_eq!(smbios_structure_size(&buf, 0), 0);
    }

    #[test]
    fn smbios_structure_size_rejects_string_set_missing_double_nul() {
        // Formatted area is complete, but the string set runs off the end of the buffer without a double-NUL terminator.
        let mut buf = vec![0x01, 0x04, 0x00, 0x00];
        buf.extend_from_slice(b"AB");
        assert_eq!(smbios_structure_size(&buf, 0), 0);
    }

    #[test]
    fn smbios_structure_size_accepts_multiple_strings() {
        let mut buf = vec![0x01, 0x04, 0x00, 0x00];
        buf.extend_from_slice(b"AAA\x00BBB\x00\x00");
        assert_eq!(smbios_structure_size(&buf, 0), buf.len());
    }

    #[test]
    fn smbios_nth_string_span_returns_none_for_out_of_range_id() {
        let mut buf = vec![0x01, 0x04, 0x00, 0x00];
        buf.extend_from_slice(b"AAA\x00\x00");
        assert_eq!(smbios_nth_string_span(&buf, 0, 4, 2), None);
    }

    #[test]
    fn smbios_nth_string_span_finds_each_string_in_multi_string_set() {
        let mut buf = vec![0x01, 0x04, 0x00, 0x00];
        buf.extend_from_slice(b"AAA\x00BBB\x00\x00");
        let first = smbios_nth_string_span(&buf, 0, 4, 1);
        let second = smbios_nth_string_span(&buf, 0, 4, 2);
        assert_eq!(first.map(|(b, e)| &buf[b..e]), Some(&b"AAA"[..]));
        assert_eq!(second.map(|(b, e)| &buf[b..e]), Some(&b"BBB"[..]));
    }

    #[test]
    fn smbios_nth_string_span_returns_none_for_unterminated_string() {
        // The string set runs off the end of the buffer before a NUL is found.
        let mut buf = vec![0x01, 0x04, 0x00, 0x00];
        buf.extend_from_slice(b"AB");
        assert_eq!(smbios_nth_string_span(&buf, 0, 4, 1), None);
    }
}
