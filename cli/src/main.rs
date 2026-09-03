/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * Copyright (c) 2025-2026 Intel Corporation
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use fs_err as fs;
use std::path::{Path, PathBuf};
use tdx_measure::{
    parse_ovmf_version, ImageConfig, Machine, QemuInfo, QemuPkg, SUPPORTED_DISTRIBUTIONS,
};

mod transcript;
use transcript::generate_transcript;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to metadata json file
    metadata: PathBuf,

    /// Enable direct boot (overrides JSON configuration)
    #[arg(long)]
    direct_boot: Option<bool>,

    /// Output JSON
    #[arg(long)]
    json: bool,

    /// Output JSON to file
    #[arg(long)]
    json_file: Option<PathBuf>,

    /// Compute MRTD and RTMR0 only
    #[arg(long)]
    platform_only: bool,

    /// Compute RTMR1 and RTMR2 only
    #[arg(long)]
    runtime_only: bool,

    /// Generate a human-readable transcript of all metadata files and write to the specified file
    #[arg(long)]
    transcript: Option<PathBuf>,

    /// Select distribution defaults for QEMU and OVMF.
    /// Supported: ubuntu:25.04, ubuntu:26.04.
    #[arg(long)]
    distribution: Option<String>,

    /// Select or override the QEMU source-package version.
    /// Currently used by --create-acpi-tables to pin the version fetched and compiled in Docker.
    /// Pass "" to fetch the current main-archive tip.
    /// Planned independent uses (not yet integrated):
    ///   - kernel patching: QEMU (pre-10.x) patches the kernel header before measuring RTMR1.
    #[arg(long)]
    qemu_version: Option<String>,

    /// QEMU package source: main | ppa:<OWNER>/<NAME> (only used with --create-acpi-tables).
    #[arg(long, requires_all = ["create_acpi_tables", "qemu_base_image"])]
    qemu_source: Option<String>,

    /// Base OCI image used to build QEMU (only used with --create-acpi-tables).
    #[arg(long, requires_all = ["create_acpi_tables", "qemu_source"])]
    qemu_base_image: Option<String>,

    /// Select or override the OVMF version (accepted: edk2-stableYYYYMM or YYYY.MM-NubuntuN[.N]).
    /// Boot#### entries: upstream OVMF 2025.05 and later measure two EV_EFI_VARIABLE_BOOT events into RTMR0.
    /// fw_cfg measurement: upstream OVMF 2025.05 and later add EV_PLATFORM_CONFIG_FLAGS events for FW_CFG.BootMenu and FW_CFG.bootorder into RTMR0.
    /// SMBIOS measurement: upstream OVMF 2025.05 and later add EV_EFI_HANDOFF_TABLES for the SMBIOS table into RTMR0.
    /// Determines initrd cmdline ordering: upstream OVMF 2025.08 and later prefix "initrd=initrd" rather than appending.
    #[arg(long)]
    ovmf_version: Option<String>,

    /// Generate ACPI tables for direct boot.
    /// Requires --distribution or both --qemu-source and --qemu-base-image.
    #[arg(long)]
    create_acpi_tables: bool,
}

/// Helper struct to resolve and store file paths
struct PathResolver {
    paths: PathStorage,
}

struct PathStorage {
    cpu_count: u8,
    memory_size: u64,
    firmware: String,
    cmdline: String,
    acpi_tables: String,
    rsdp: Option<String>,
    table_loader: Option<String>,
    boot_order: Option<String>,
    path_boot_xxxx: Option<String>,
    smbios: Option<String>,
    // Direct boot specific
    kernel: Option<String>,
    initrd: Option<String>,
    // Indirect boot specific
    qcow2: Option<String>,
    mok_list: Option<String>,
    mok_list_trusted: Option<String>,
    mok_list_x: Option<String>,
    sbat_level: Option<String>,
}

impl PathResolver {
    fn new(
        metadata_path: &Path,
        image_config: &ImageConfig,
        require_boot_config: bool,
    ) -> Result<Self> {
        let parent_dir = metadata_path.parent().unwrap_or(".".as_ref());

        // Handle optional boot_config
        let paths = if let Some(boot_config) = &image_config.boot_config {
            PathStorage {
                cpu_count: boot_config.cpus,
                memory_size: image_config.memory_size()?,
                firmware: parent_dir.join(&boot_config.bios).display().to_string(),
                cmdline: image_config.cmdline().to_string(),
                acpi_tables: parent_dir
                    .join(&boot_config.acpi_tables)
                    .display()
                    .to_string(),
                rsdp: boot_config
                    .rsdp
                    .as_ref()
                    .map(|p| parent_dir.join(p).display().to_string()),
                table_loader: boot_config
                    .table_loader
                    .as_ref()
                    .map(|p| parent_dir.join(p).display().to_string()),
                boot_order: boot_config
                    .boot_order
                    .as_ref()
                    .map(|p| parent_dir.join(p).display().to_string()),
                path_boot_xxxx: boot_config
                    .path_boot_xxxx
                    .as_ref()
                    .map(|p| parent_dir.join(p).display().to_string()),
                smbios: boot_config
                    .smbios
                    .as_ref()
                    .map(|p| parent_dir.join(p).display().to_string()),
                kernel: image_config
                    .direct_boot()
                    .map(|d| parent_dir.join(&d.kernel).display().to_string()),
                initrd: image_config
                    .direct_boot()
                    .map(|d| parent_dir.join(&d.initrd).display().to_string()),
                qcow2: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.qcow2).display().to_string()),
                mok_list: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.mok_list).display().to_string()),
                mok_list_trusted: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.mok_list_trusted).display().to_string()),
                mok_list_x: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.mok_list_x).display().to_string()),
                sbat_level: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.sbat_level).display().to_string()),
            }
        } else {
            // When boot_config is None (runtime-only mode), provide empty strings for platform fields
            if require_boot_config {
                return Err(anyhow!(
                    "Boot info is required but not provided in the configuration"
                ));
            }
            PathStorage {
                cpu_count: 0,
                memory_size: 0,
                firmware: String::new(),
                cmdline: image_config.cmdline().to_string(),
                acpi_tables: String::new(),
                rsdp: None,
                table_loader: None,
                boot_order: None,
                path_boot_xxxx: None,
                smbios: None,
                kernel: image_config
                    .direct_boot()
                    .map(|d| parent_dir.join(&d.kernel).display().to_string()),
                initrd: image_config
                    .direct_boot()
                    .map(|d| parent_dir.join(&d.initrd).display().to_string()),
                qcow2: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.qcow2).display().to_string()),
                mok_list: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.mok_list).display().to_string()),
                mok_list_trusted: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.mok_list_trusted).display().to_string()),
                mok_list_x: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.mok_list_x).display().to_string()),
                sbat_level: image_config
                    .indirect_boot()
                    .map(|i| parent_dir.join(&i.sbat_level).display().to_string()),
            }
        };

        Ok(Self { paths })
    }

    fn build_machine<'a>(
        &'a self,
        direct_boot: bool,
        metadata_path: &'a Path,
        create_acpi_table: bool,
        qemu_info: QemuInfo<'a>,
        ovmf_version: Option<(u16, u8)>,
    ) -> Machine<'a> {
        Machine::builder()
            .cpu_count(self.paths.cpu_count)
            .memory_size(self.paths.memory_size)
            .firmware(&self.paths.firmware)
            .kernel_cmdline(&self.paths.cmdline)
            .acpi_tables(&self.paths.acpi_tables)
            .rsdp(self.paths.rsdp.as_deref().unwrap_or(""))
            .table_loader(self.paths.table_loader.as_deref().unwrap_or(""))
            .boot_order(self.paths.boot_order.as_deref().unwrap_or(""))
            .path_boot_xxxx(self.paths.path_boot_xxxx.as_deref().unwrap_or(""))
            .maybe_smbios(self.paths.smbios.as_deref())
            .kernel(self.paths.kernel.as_deref().unwrap_or(""))
            .initrd(self.paths.initrd.as_deref().unwrap_or(""))
            .qcow2(self.paths.qcow2.as_deref().unwrap_or(""))
            .mok_list(self.paths.mok_list.as_deref().unwrap_or(""))
            .mok_list_trusted(self.paths.mok_list_trusted.as_deref().unwrap_or(""))
            .mok_list_x(self.paths.mok_list_x.as_deref().unwrap_or(""))
            .sbat_level(self.paths.sbat_level.as_deref().unwrap_or(""))
            .direct_boot(direct_boot)
            .metadata_path(metadata_path)
            .create_acpi_table(create_acpi_table)
            .maybe_qemu_version(qemu_info.version)
            .maybe_qemu_source(qemu_info.source)
            .maybe_qemu_base_image(qemu_info.base_image)
            .maybe_ovmf_version(ovmf_version)
            .build()
    }
}

/// Resolves the QEMU source, version, and base image to fetch/build for a given distribution, applying pinned per-distribution defaults and honoring any explicit overrides.
fn qemu_pkg_for<'a>(distribution: &'a str, qemu_info: QemuInfo<'a>) -> Result<QemuPkg<'a>> {
    if distribution.is_empty() {
        // Case 1: no --distribution given, so every QEMU parameter must come from an explicit override.
        let source = qemu_info.source.ok_or_else(|| {
            anyhow!("--qemu-source is required when --distribution is not specified")
        })?;
        let version = qemu_info.version.unwrap_or("");
        let base_image = qemu_info.base_image.ok_or_else(|| {
            anyhow!("--qemu-base-image is required when --distribution is not specified")
        })?;
        Ok(QemuPkg {
            source,
            version,
            base_image,
        })
    } else {
        let def = SUPPORTED_DISTRIBUTIONS.find(distribution).ok_or_else(|| {
            anyhow!(
                "unsupported distribution: {distribution}. Supported: {}",
                SUPPORTED_DISTRIBUTIONS.join(", ")
            )
        })?;

        // Case 2: --distribution given and a field has no override -> use that distribution's pinned default.
        // Case 3: --distribution given and a field has an override -> the override wins for that field.
        Ok(QemuPkg {
            source: qemu_info.source.unwrap_or(def.qemu_source),
            version: qemu_info.version.unwrap_or(def.qemu_default_version),
            base_image: qemu_info.base_image.unwrap_or(def.qemu_base_image),
        })
    }
}

fn process_measurements(config: &Cli, image_config: &ImageConfig) -> Result<()> {
    // Validate the configuration
    image_config
        .validate()
        .map_err(|e| anyhow!("Invalid image configuration: {}", e))?;

    // Determine boot mode: CLI flag overrides JSON configuration, defaults to direct boot
    let cli_direct_boot = config.direct_boot;
    let has_indirect_boot = image_config.indirect_boot().is_some();
    let direct_boot = match cli_direct_boot {
        Some(value) => value,
        None => !has_indirect_boot,
    };

    // Validate boot mode configuration (skip validation for platform-only mode)
    if !config.platform_only {
        match (
            direct_boot,
            image_config.direct_boot(),
            image_config.indirect_boot(),
        ) {
            (true, None, _) => {
                return Err(anyhow!(
                    "Direct boot mode specified but no direct boot configuration found in JSON"
                ))
            }
            (false, _, None) => {
                return Err(anyhow!(
                    "Indirect boot mode specified but no indirect boot configuration found in JSON"
                ))
            }
            _ => {}
        }
    }

    // Build machine
    let path_resolver = PathResolver::new(&config.metadata, image_config, !config.runtime_only)?;
    let create_acpi_table = config.create_acpi_tables;
    let qemu_version = config.qemu_version.as_deref();
    let qemu_source = config.qemu_source.as_deref();
    let qemu_base_image = config.qemu_base_image.as_deref();
    let mut error_msgs = String::new();

    // Validate --distribution
    let distribution = config.distribution.as_deref();
    if let Some(distro) = distribution {
        if SUPPORTED_DISTRIBUTIONS.find(distro).is_none() {
            return Err(anyhow!(
                "Unsupported distribution '{}'. Supported distributions: {}",
                distro,
                SUPPORTED_DISTRIBUTIONS.join(", ")
            ));
        }
    }

    // If both --ovmf-version and --distribution are given, --ovmf-version takes precedence over the distribution's default OVMF version.
    let ovmf_version: Option<(u16, u8)> = if let Some(ver) = config.ovmf_version.as_deref() {
        Some(parse_ovmf_version(ver).ok_or_else(|| anyhow!(
            "OVMF version '{}' is invalid. Use OVMF upstream version tag in the format edk2-stableYYYYMM or an Ubuntu package version tag in the format YYYY.MM-NubuntuN[.N]",
            ver
        ))?)
    } else if let Some(distro) = distribution {
        let default = SUPPORTED_DISTRIBUTIONS
            .default_ovmf_version(distro)
            .ok_or_else(|| anyhow!("no default OVMF version for distribution '{}'", distro))?;
        Some(parse_ovmf_version(default).ok_or_else(|| anyhow!(
            "Distribution default OVMF version '{}' for '{}' is invalid. Use OVMF upstream version tag in the format edk2-stableYYYYMM or an Ubuntu package version tag in the format YYYY.MM-NubuntuN[.N]",
            default,
            distro
        ))?)
    } else {
        None
    };

    // Validate --qemu-source and --qemu-base-image
    if let Some(src) = qemu_source {
        let valid_ppa = src
            .strip_prefix("ppa:")
            .and_then(|rest| rest.split_once('/'))
            .map(|(owner, name)| !owner.is_empty() && !name.is_empty())
            .unwrap_or(false);
        if src != "main" && !valid_ppa {
            return Err(anyhow!(
                "--qemu-source must be 'main' or 'ppa:<OWNER>/<NAME>' (got '{src}')"
            ));
        }
    }
    if let Some(img) = qemu_base_image {
        let valid = match img.split_once("@sha256:") {
            Some((_, digest)) => {
                digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit())
            }
            None => false,
        };
        if !valid {
            return Err(anyhow!(
                "--qemu-base-image must be a full reference including a 64-char sha256 digest (e.g. ubuntu:26.04@sha256:...) (got '{img}')"
            ));
        }
    }

    // Resolve the QEMU package to fetch/build for ACPI generation.
    // This is only meaningful when the ACPI table will be created.
    let mut qemu_info = QemuInfo {
        version: qemu_version,
        source: qemu_source,
        base_image: qemu_base_image,
    };

    // Check usage of --create-acpi-tables flag
    if create_acpi_table {
        if config.runtime_only {
            // Runtime-only measurements do not use platform configuration or generate ACPI tables.
            eprintln!(
                "--create-acpi-tables is not required with --runtime-only and will be ignored"
            );
        } else {
            // Validate that --create-acpi-tables is only used with direct boot.
            if !direct_boot {
                error_msgs.push_str("--create-acpi-tables is not valid with indirect boot\n");
            }

            // Validate that --create-acpi-tables is only used with a supported distribution or both --qemu-source and --qemu-base-image.
            if distribution.is_none() && (qemu_source.is_none() || qemu_base_image.is_none()) {
                return Err(anyhow!("--create-acpi-tables requires --distribution or both --qemu-source and --qemu-base-image"));
            }

            let pkg = qemu_pkg_for(distribution.unwrap_or(""), qemu_info)?;
            qemu_info = QemuInfo {
                version: Some(pkg.version),
                source: Some(pkg.source),
                base_image: Some(pkg.base_image),
            };
        }
    }

    // Check usage of ACPI table path
    if !config.runtime_only {
        let acpi_tables_path = Path::new(&path_resolver.paths.acpi_tables);
        let acpi_tables_path_missing = !acpi_tables_path.exists();
        if acpi_tables_path_missing {
            if !direct_boot {
                error_msgs.push_str(&format!("The ACPI tables file path must be provided in metadata.json and the file must exist. Path: {}\n", acpi_tables_path.display()));
            } else if direct_boot && !create_acpi_table {
                error_msgs.push_str(&format!("Either use --create-acpi-tables flag to generate the ACPI tables or ensure the ACPI tables file path is provided in metadata.json and the file exists. Path: {}\n", acpi_tables_path.display()));
            }
        }
    }

    if !error_msgs.is_empty() {
        return Err(anyhow!(error_msgs.trim_end().to_owned()));
    }

    // Direct boot measurement requires --distribution or --ovmf-version.
    if direct_boot && !config.platform_only && distribution.is_none() && ovmf_version.is_none() {
        return Err(anyhow!(
            "--distribution or --ovmf-version is required for direct boot measurement. \
             Supported distributions: {}",
            SUPPORTED_DISTRIBUTIONS.join(", ")
        ));
    }

    let machine = path_resolver.build_machine(
        direct_boot,
        &config.metadata,
        create_acpi_table,
        qemu_info,
        ovmf_version,
    );

    // Measure
    let measurements = if config.platform_only {
        machine
            .measure_platform()
            .context("Failed to measure platform")?
    } else if config.runtime_only {
        machine
            .measure_runtime()
            .context("Failed to measure runtime")?
    } else {
        machine
            .measure()
            .context("Failed to measure machine configuration")?
    };

    // Generate transcript (if requested).
    if let Some(ref transcript_file) = config.transcript {
        generate_transcript(
            transcript_file,
            &path_resolver,
            direct_boot,
            config.platform_only,
            config.runtime_only,
            ovmf_version,
        )?;
    }

    // Output results
    output_measurements(config, &measurements)?;

    Ok(())
}

fn output_measurements(config: &Cli, measurements: &tdx_measure::TdxMeasurements) -> Result<()> {
    let json_output = serde_json::to_string_pretty(measurements).unwrap();

    if config.json {
        println!("{}", json_output);
    } else {
        println!("Machine measurements:");
        println!("MRTD: {}", hex::encode(&measurements.mrtd));
        println!("RTMR0: {}", hex::encode(&measurements.rtmr0));
        println!("RTMR1: {}", hex::encode(&measurements.rtmr1));
        println!("RTMR2: {}", hex::encode(&measurements.rtmr2));
    }

    if let Some(ref json_file) = config.json_file {
        fs::write(json_file, json_output).context("Failed to write measurements to file")?;
    }

    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let metadata = fs::read_to_string(&cli.metadata).context("Failed to read image metadata")?;
    let image_config: ImageConfig =
        serde_json::from_str(&metadata).context("Failed to parse image metadata")?;

    process_measurements(&cli, &image_config)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for constructing `QemuInfo` from three plain `Option<&str>` values in tests.
    fn qemu_info<'a>(
        version: Option<&'a str>,
        source: Option<&'a str>,
        base_image: Option<&'a str>,
    ) -> QemuInfo<'a> {
        QemuInfo {
            version,
            source,
            base_image,
        }
    }

    #[test]
    fn qemu_pkg_for_known_distros_returns_pinned_defaults() {
        let p = qemu_pkg_for("ubuntu:25.04", qemu_info(None, None, None)).unwrap();
        assert_eq!(p.source, "ppa:kobuk-team/tdx-release");
        assert_eq!(p.version, "1:9.2.1+ds-1ubuntu4+tdx2.0~ppa2");
        assert!(p.base_image.contains("@sha256:"));

        let p = qemu_pkg_for("ubuntu:26.04", qemu_info(None, None, None)).unwrap();
        assert_eq!(p.source, "main");
        assert_eq!(p.version, "1:10.2.1+ds-1ubuntu4");
        assert!(p.base_image.contains("@sha256:"));
    }

    #[test]
    fn qemu_pkg_for_rejects_unknown_distro() {
        assert!(qemu_pkg_for("debian:12", qemu_info(None, None, None)).is_err());
    }

    #[test]
    fn qemu_pkg_for_honors_version_override() {
        let p = qemu_pkg_for(
            "ubuntu:26.04",
            qemu_info(Some("1:10.3.0-1ubuntu1"), None, None),
        )
        .unwrap();
        assert_eq!(p.source, "main");
        assert_eq!(p.version, "1:10.3.0-1ubuntu1");

        // Even an empty override (caller asked for "latest") wins over the pin.
        let p = qemu_pkg_for("ubuntu:26.04", qemu_info(Some(""), None, None)).unwrap();
        assert_eq!(p.version, "");
    }

    #[test]
    fn qemu_pkg_for_honors_source_and_base_image_overrides() {
        let p = qemu_pkg_for(
            "ubuntu:26.04",
            qemu_info(
                Some("1:10.3.0-1ubuntu1"),
                Some("ppa:my-team/my-ppa"),
                Some("ubuntu:26.04@sha256:abc123"),
            ),
        )
        .unwrap();
        assert_eq!(p.source, "ppa:my-team/my-ppa");
        assert_eq!(p.version, "1:10.3.0-1ubuntu1");
        assert_eq!(p.base_image, "ubuntu:26.04@sha256:abc123");
    }

    #[test]
    fn qemu_pkg_for_without_distribution_requires_source_and_base_image() {
        // Missing both source and base_image
        assert!(qemu_pkg_for("", qemu_info(None, None, None)).is_err());
        // Missing base_image
        assert!(qemu_pkg_for("", qemu_info(Some("1.0"), Some("main"), None)).is_err());
        // Missing source
        assert!(qemu_pkg_for("", qemu_info(Some("1.0"), None, Some("img@sha256:abc"))).is_err());
        // All provided - should succeed
        let p = qemu_pkg_for(
            "",
            qemu_info(Some("1.0"), Some("main"), Some("ubuntu:26.04@sha256:abc")),
        )
        .unwrap();
        assert_eq!(p.source, "main");
        assert_eq!(p.version, "1.0");
        assert_eq!(p.base_image, "ubuntu:26.04@sha256:abc");
    }
}
