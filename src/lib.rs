/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * Copyright (c) 2025 Intel Corporation
 * SPDX-License-Identifier: Apache-2.0
 */
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_human_bytes as hex_bytes;

pub use machine::Machine;

use util::{measure_log, measure_sha384};

mod acpi;
mod image;
mod kernel;
mod machine;
mod num;
mod tdvf;
mod util;

/// Package metadata for a single supported distribution.
pub struct DistributionDef {
    pub name: &'static str,
    pub qemu_source: &'static str,
    pub qemu_default_version: &'static str,
    pub qemu_base_image: &'static str,
    pub ovmf_default_version: &'static str,
}

/// The complete, authoritative set of distributions this tool supports.
/// Use `SUPPORTED_DISTRIBUTIONS` (the crate-level constant of this type) to validate CLI input, build error messages, and look up package metadata.
pub struct SupportedDistributions(&'static [DistributionDef]);

impl SupportedDistributions {
    /// Iterates over all supported distribution name strings.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.0.iter().map(|d| d.name)
    }

    /// Joins all distribution names with `sep` (e.g. `", "`).
    pub fn join(&self, sep: &str) -> String {
        self.names().collect::<Vec<_>>().join(sep)
    }

    /// Returns the distribution definition for `name`, or `None` if not supported.
    pub fn find(&self, name: &str) -> Option<&'static DistributionDef> {
        self.0.iter().find(|d| d.name == name)
    }

    /// Return the default OVMF version for a given supported distribution.
    pub fn default_ovmf_version(&self, distribution: &str) -> Option<&'static str> {
        self.find(distribution).map(|def| def.ovmf_default_version)
    }
}

/// Single source of truth for every distribution this tool supports.
/// All data (names, source selectors, pinned versions, OCI base images) lives here.
pub const SUPPORTED_DISTRIBUTIONS: SupportedDistributions = SupportedDistributions(&[
    DistributionDef {
        name: "ubuntu:25.04",
        qemu_source: "ppa:kobuk-team/tdx-release",
        qemu_default_version: "1:9.2.1+ds-1ubuntu4+tdx2.0~ppa2",
        qemu_base_image:
            "ubuntu:25.04@sha256:27771fb7b40a58237c98e8d3e6b9ecdd9289cec69a857fccfb85ff36294dac20",
        ovmf_default_version: "2025.02-3ubuntu2.2",
    },
    DistributionDef {
        name: "ubuntu:26.04",
        qemu_source: "main",
        qemu_default_version: "1:10.2.1+ds-1ubuntu4",
        qemu_base_image:
            "ubuntu:26.04@sha256:f3d28607ddd78734bb7f71f117f3c6706c666b8b76cbff7c9ff6e5718d46ff64",
        ovmf_default_version: "2025.02-3ubuntu2.2",
    },
]);

/// Parses version information from upstream OVMF tag or Ubuntu OVMF package tag.
/// Returns (year, month) if successful and None if the input is invalid.
pub fn parse_ovmf_version(version: &str) -> Option<(u16, u8)> {
    // Parse upstream OVMF version tag, which have the form 'edk2-stableYYYYMM'.
    if let Some(release) = version.strip_prefix("edk2-stable") {
        // The upstream release payload must be exactly six digits for YYYYMM.
        if release.len() != 6 || !release.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }

        // The first four digits encode the year and the last two encode the month.
        let year = release[..4].parse().ok()?;
        let month: u8 = release[4..].parse().ok()?;

        // Check valid month range.
        if month == 0 || month > 12 {
            return None;
        }
        return Some((year, month));
    }

    // Parse Ubuntu OVMF package tag, which have the form 'YYYY.MM-<debian_revision>ubuntu<ubuntu_revision>'.
    // The part before '-' carries the upstream release we want to extract.
    let (release, package_revision) = version.split_once('-')?;

    // The Ubuntu release prefix must be exactly YYYY.MM.
    let (year_text, month_text) = release.split_once('.')?;
    if year_text.len() != 4
        || month_text.len() != 2
        || !year_text.chars().all(|c| c.is_ascii_digit())
        || !month_text.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }

    // The parsed month must be in the inclusive range 1..=12.
    let year = year_text.parse().ok()?;
    let month: u8 = month_text.parse().ok()?;
    if month == 0 || month > 12 {
        return None;
    }

    // Package revisions are expected to contain exactly one 'ubuntu' separator.
    let mut revision_parts = package_revision.split("ubuntu");
    let debian_revision = revision_parts.next()?;
    let ubuntu_revision = revision_parts.next()?;

    // Debian revision must be numeric and Ubuntu revision must be present.
    if revision_parts.next().is_some()
        || debian_revision.is_empty()
        || !debian_revision.chars().all(|c| c.is_ascii_digit())
        || ubuntu_revision.is_empty()
    {
        return None;
    }

    // Ubuntu revision can include dotted numeric updates such as '7.1'.
    for segment in ubuntu_revision.split('.') {
        if segment.is_empty() || !segment.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
    }

    Some((year, month))
}

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
    pub qemu: Option<QemuShape>,
}

/// QEMU command-line pass-through consumed by the ACPI dumper.
/// Device-ordering matters. See README for the field-by-field schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QemuShape {
    pub machine: String,
    #[serde(default = "default_cpu")]
    pub cpu: String,
    #[serde(default = "default_accel")]
    pub accel: String,
    #[serde(default)]
    pub globals: Vec<String>,
    #[serde(default)]
    pub objects: Vec<String>,
    #[serde(default)]
    pub netdevs: Vec<String>,
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default)]
    pub fw_cfg: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_serial")]
    pub serial: Option<String>,
}

fn default_cpu() -> String {
    "host".to_string()
}
fn default_accel() -> String {
    "kvm".to_string()
}

// Rejects anything other than "stdio".
fn deserialize_serial<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<String> = Option::deserialize(deserializer)?;
    match value {
        Some(s) if s != "stdio" => Err(serde::de::Error::custom(format!(
            "unsupported value {s:?} for `serial`: only \"stdio\" is currently supported"
        ))),
        other => Ok(other),
    }
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
        let boot_config = self
            .boot_config
            .as_ref()
            .ok_or_else(|| anyhow!("Boot config is required"))?;

        Ok(boot_config.cpus)
    }

    /// Get memory size from boot config
    pub fn memory_size(&self) -> Result<u64> {
        let boot_config = self
            .boot_config
            .as_ref()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns true if `image` ends with `@sha256:` followed by exactly 64 hex characters.
    fn has_pinned_digest(image: &str) -> bool {
        match image.split_once("@sha256:") {
            Some((_, digest)) => {
                digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit())
            }
            None => false,
        }
    }

    #[test]
    fn supported_distributions_find_returns_well_formed_definition_for_every_entry() {
        for name in SUPPORTED_DISTRIBUTIONS.names() {
            let def = SUPPORTED_DISTRIBUTIONS.find(name).unwrap();
            assert_eq!(def.name, name);
            assert!(
                def.qemu_source == "main" || def.qemu_source.starts_with("ppa:"),
                "qemu_source for {name} must be 'main' or 'ppa:<owner>/<name>', got '{}'",
                def.qemu_source
            );
            assert!(
                !def.qemu_default_version.is_empty(),
                "qemu_default_version for {name} must not be empty"
            );
            assert!(
                has_pinned_digest(def.qemu_base_image),
                "qemu_base_image for {name} must include a 64-char sha256 digest, got '{}'",
                def.qemu_base_image
            );
            assert!(
                !def.ovmf_default_version.is_empty(),
                "ovmf_default_version for {name} must not be empty"
            );
        }
    }

    #[test]
    fn supported_distributions_find_returns_none_for_unknown() {
        assert!(SUPPORTED_DISTRIBUTIONS.find("fedora:40").is_none());
    }

    #[test]
    fn supported_distributions_join_uses_separator() {
        assert_eq!(
            SUPPORTED_DISTRIBUTIONS.join(", "),
            "ubuntu:25.04, ubuntu:26.04"
        );
        assert_eq!(
            SUPPORTED_DISTRIBUTIONS.join("|"),
            "ubuntu:25.04|ubuntu:26.04"
        );
    }

    #[test]
    fn supported_distributions_names_iterates() {
        let names: Vec<_> = SUPPORTED_DISTRIBUTIONS.names().collect();
        assert!(names.contains(&"ubuntu:25.04"));
        assert!(names.contains(&"ubuntu:26.04"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn default_ovmf_version_returns_none_for_unknown_distro() {
        assert_eq!(
            SUPPORTED_DISTRIBUTIONS.default_ovmf_version("fedora:40"),
            None
        );
    }

    #[test]
    fn parses_upstream_stable_tag_and_ubuntu_ovmf_versions() {
        assert_eq!(parse_ovmf_version("edk2-stable202511"), Some((2025, 11)));
        assert_eq!(parse_ovmf_version("2025.11-3ubuntu7.1"), Some((2025, 11)));
    }

    #[test]
    fn rejects_ambiguous_ovmf_version_names() {
        for version in [
            "2025",
            "2025.11",
            "2025.13",
            "edk2-stable2025.11",
            "2025.11-ubuntu7",
            "v2025.11",
            "2025.11-3ubuntu",
            "2025.11-3ubuntu7.",
            "2025.11-3ubuntu7.x",
        ] {
            assert_eq!(
                parse_ovmf_version(version),
                None,
                "unexpectedly accepted {version}"
            );
        }
    }

    #[test]
    fn default_ovmf_version_returns_value_for_known_distros() {
        for name in SUPPORTED_DISTRIBUTIONS.names() {
            assert!(
                SUPPORTED_DISTRIBUTIONS.default_ovmf_version(name).is_some(),
                "expected Some for known distro '{name}', got None"
            );
        }
    }

    #[test]
    fn every_distribution_default_ovmf_version_is_parseable() {
        for name in SUPPORTED_DISTRIBUTIONS.names() {
            let def_ver = SUPPORTED_DISTRIBUTIONS.default_ovmf_version(name).unwrap();
            assert!(
                parse_ovmf_version(def_ver).is_some(),
                "default OVMF version '{def_ver}' for {name} is not parseable"
            );
        }
    }
}
