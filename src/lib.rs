/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * SPDX-License-Identifier: Apache-2.0
 */
use serde::{Deserialize, Serialize};
use serde_human_bytes as hex_bytes;
use anyhow::{anyhow, Result};

pub use machine::Machine;

use util::{measure_log, measure_sha384};

mod acpi;
mod kernel;
mod image;
mod machine;
mod num;
mod tdvf;
mod util;

/// Contains all the measurement values for TDX.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TdxMeasurements {
    #[serde(with = "hex_bytes")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mrtd: Vec<u8>,
    #[serde(with = "hex_bytes")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rtmr0: Vec<u8>,
    #[serde(with = "hex_bytes")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rtmr1: Vec<u8>,
    #[serde(with = "hex_bytes")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rtmr2: Vec<u8>,
}

/// Common boot configuration (platform-specific)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootConfig {
    pub cpus: u8,
    pub memory: String,
    pub bios: String,
    pub acpi_tables: String,
    pub rsdp: Option<String>,
    pub table_loader: Option<String>,
    pub boot_order: Option<String>,
    pub path_boot_xxxx: Option<String>,
}

/// Direct boot specific information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectBoot {
    pub kernel: String,
    pub initrd: String,
    pub cmdline: String,
}

/// Indirect boot specific information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndirectBoot {
    pub qcow2: String,
    pub cmdline: String,
    pub mok_list: String,
    pub mok_list_trusted: String,
    pub mok_list_x: String,
    pub sbat_level: String,
}

/// Complete image configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_config: Option<BootConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct: Option<DirectBoot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indirect: Option<IndirectBoot>,
}

impl ImageConfig {
    /// Validate that exactly one boot mode is specified
    pub fn validate(&self) -> Result<(), String> {
        match (&self.direct, &self.indirect) {
            (Some(_), None) => Ok(()),
            (None, Some(_)) => Ok(()),
            (Some(_), Some(_)) => Err("Cannot specify both direct and indirect boot".to_string()),
            (None, None) => Ok(()),
        }
    }

    /// Check if this is direct boot mode
    pub fn is_direct_boot(&self) -> bool {
        self.direct.is_some()
    }

    /// Get direct boot info
    pub fn direct_boot(&self) -> Option<&DirectBoot> {
        self.direct.as_ref()
    }

    /// Get indirect boot info
    pub fn indirect_boot(&self) -> Option<&IndirectBoot> {
        self.indirect.as_ref()
    }

    /// Get command line regardless of boot mode
    pub fn cmdline(&self) -> &str {
        match (&self.direct, &self.indirect) {
            (Some(direct), None) => &direct.cmdline,
            (None, Some(indirect)) => &indirect.cmdline,
            _ => "", // Should not happen after validation
        }
    }

    /// Get CPU count from boot config
    pub fn cpu_count(&self) -> Result<u8> {
        let boot_config = self.boot_config.as_ref()
            .ok_or_else(|| anyhow!("Boot config is required"))?;

        Ok(boot_config.cpus)
    }

    /// Get memory size from boot config
    pub fn memory_size(&self) -> Result<u64> {
        let boot_config = self.boot_config.as_ref()
            .ok_or_else(|| anyhow!("Boot config is required"))?;

        parse_memory_size(&boot_config.memory)
    }
}

/// Parse a memory size value that can be decimal or hexadecimal (with 0x prefix)
pub fn parse_memory_size(s: &str) -> Result<u64> {
    let s = s.trim();

    if s.is_empty() {
        return Err(anyhow!("Empty memory size"));
    }
    if s.starts_with("0x") || s.starts_with("0X") {
        let hex_str = &s[2..];
        return u64::from_str_radix(hex_str, 16)
            .map_err(|e| anyhow!("Invalid hexadecimal value: {}", e));
    }

    if s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(s.parse::<u64>()?);
    }
    let len = s.len();
    let (num_part, suffix) = match s.chars().last().unwrap() {
        'k' | 'K' => (&s[0..len - 1], 1024u64),
        'm' | 'M' => (&s[0..len - 1], 1024u64 * 1024),
        'g' | 'G' => (&s[0..len - 1], 1024u64 * 1024 * 1024),
        't' | 'T' => (&s[0..len - 1], 1024u64 * 1024 * 1024 * 1024),
        _ => return Err(anyhow!("Unknown memory size suffix")),
    };

    let num = num_part.parse::<u64>()?;
    Ok(num * suffix)
}
