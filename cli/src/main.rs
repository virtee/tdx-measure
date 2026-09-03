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
    parse_ovmf_version, resolve_qemu_archive_version, ImageConfig, Machine, QemuInfo, QemuPkg,
    SUPPORTED_DISTRIBUTIONS,
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

/// Resolves the concrete QEMU version to feed into the Machine from the CLI flags.
/// Returns `None` when no version applies (e.g. platform-only without ACPI generation, or indirect boot without a version).
/// When `--qemu-version ""` is given for direct boot, calls `resolve_tip` (the Docker fetcher in production) to learn the archive tip; an explicit version is returned as-is; an absent flag falls back to the distribution default.
/// The archive-tip resolver is injected so tests can stub out Docker.
fn resolve_qemu_version_from_cli(
    config: &Cli,
    direct_boot: bool,
    resolve_tip: impl FnOnce(&QemuPkg) -> Result<String>,
) -> Result<Option<String>> {
    let qemu_version_flag = config.qemu_version.as_deref();
    let distribution = config.distribution.as_deref();
    let qemu_source = config.qemu_source.as_deref();
    let qemu_base_image = config.qemu_base_image.as_deref();
    let platform_only = config.platform_only;

    // Direct boot measurement requires either a version or a distribution.
    if direct_boot && !platform_only && qemu_version_flag.is_none() && distribution.is_none() {
        return Err(anyhow!(
            "--distribution or --qemu-version is required for direct boot measurement. \
             Supported distributions: {}",
            SUPPORTED_DISTRIBUTIONS.join(", ")
        ));
    }

    // An explicit empty string means "resolve the current archive tip" rather than "no version".
    if qemu_version_flag == Some("") {
        // No QEMU version is needed outside direct boot, or for platform-only runs that don't build QEMU to generate ACPI tables.
        if !direct_boot || (platform_only && !acpi_generation_enabled(config)) {
            return Ok(None);
        }

        // Resolving the archive tip requires a way to fetch/build QEMU: either a distribution or an explicit source/base-image pair.
        if distribution.is_none() && qemu_source.is_none() && qemu_base_image.is_none() {
            return Err(anyhow!(
                "--distribution is required when --qemu-version is empty (archive tip), \
                 or provide both --qemu-source and --qemu-base-image. \
                 Supported distributions: {}",
                SUPPORTED_DISTRIBUTIONS.join(", ")
            ));
        }

        // Without --distribution, both --qemu-source and --qemu-base-image must be given together.
        if distribution.is_none() && (qemu_source.is_none() || qemu_base_image.is_none()) {
            let missing = match (qemu_source.is_none(), qemu_base_image.is_none()) {
                (true, false) => "--qemu-source",
                (false, true) => "--qemu-base-image",
                _ => "--qemu-source and --qemu-base-image",
            };
            return Err(anyhow!(
                "If --distribution is omitted, {missing} is also required. \
                 Supported distributions: {}",
                SUPPORTED_DISTRIBUTIONS.join(", ")
            ));
        }

        // Build the package descriptor for the tip resolution, applying distribution defaults for any field left unset.
        let pkg = qemu_pkg_for(
            distribution.unwrap_or(""),
            QemuInfo {
                version: Some(""),
                source: qemu_source,
                base_image: qemu_base_image,
            },
        )?;

        // Actually fetch/build QEMU (via Docker) to learn the concrete tip version.
        return Ok(Some(
            resolve_tip(&pkg).context("Failed to resolve QEMU archive tip version")?,
        ));
    }

    Ok(match qemu_version_flag {
        // An explicit non-empty version always wins.
        Some(ver) if !ver.is_empty() => Some(ver.to_string()),

        // Otherwise fall back to the distribution's pinned default, if any.
        _ => distribution
            .and_then(|d| SUPPORTED_DISTRIBUTIONS.find(d))
            .map(|def| def.qemu_default_version.to_string()),
    })
}

/// Whether this run will generate ACPI tables (runtime-only runs ignore `--create-acpi-tables`).
fn acpi_generation_enabled(config: &Cli) -> bool {
    config.create_acpi_tables && !config.runtime_only
}

/// Builds the QEMU package handed to the ACPI table build, pinned to `resolved_version`.
/// Returns `None` when this run does not generate ACPI tables.
fn qemu_pkg_for_acpi_generation<'a>(
    config: &'a Cli,
    direct_boot: bool,
    resolved_version: Option<&'a str>,
) -> Result<Option<QemuPkg<'a>>> {
    if !(acpi_generation_enabled(config) && direct_boot) {
        return Ok(None);
    }
    qemu_pkg_for(
        config.distribution.as_deref().unwrap_or(""),
        QemuInfo {
            version: resolved_version,
            source: config.qemu_source.as_deref(),
            base_image: config.qemu_base_image.as_deref(),
        },
    )
    .map(Some)
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

    // Resolve the concrete QEMU version (running the Docker fetcher for `--qemu-version ""`) so the same version drives ACPI generation and, later, the kernel-patch decision.
    let resolved_qemu_version =
        resolve_qemu_version_from_cli(config, direct_boot, resolve_qemu_archive_version)?;

    // When ACPI tables will actually be generated, resolve the full QEMU package (applying distribution defaults) up front so `Machine` receives concrete values instead of raw overrides.
    let qemu_info = match qemu_pkg_for_acpi_generation(
        config,
        direct_boot,
        resolved_qemu_version.as_deref(),
    )? {
        Some(pkg) => QemuInfo {
            version: Some(pkg.version),
            source: Some(pkg.source),
            base_image: Some(pkg.base_image),
        },
        None => QemuInfo {
            version: resolved_qemu_version.as_deref(),
            source: qemu_source,
            base_image: qemu_base_image,
        },
    };

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

    /// Archive-tip resolver stub for paths that must never run the Docker fetcher.
    fn no_docker(_: &QemuPkg) -> Result<String> {
        unreachable!("must not hit Docker")
    }

    /// A `Cli` with every flag unset, to be customised with struct-update syntax.
    /// Built directly rather than via `Cli::parse_from` so tests can exercise flag combinations that clap would reject.
    fn base_cli() -> Cli {
        Cli {
            metadata: PathBuf::from("metadata.json"),
            direct_boot: None,
            json: false,
            json_file: None,
            platform_only: false,
            runtime_only: false,
            transcript: None,
            distribution: None,
            qemu_version: None,
            qemu_source: None,
            qemu_base_image: None,
            ovmf_version: None,
            create_acpi_tables: false,
        }
    }

    #[test]
    fn resolve_qemu_version_requires_distribution_or_version_for_direct_boot() {
        let err = resolve_qemu_version_from_cli(&base_cli(), true, no_docker).unwrap_err();
        assert!(err
            .to_string()
            .contains("required for direct boot measurement"));
    }

    #[test]
    fn resolve_qemu_version_direct_boot_requirement_bypassed_by_platform_only() {
        // platform_only=true negates the guard's `!platform_only` term, so no error is raised even with no flag or distribution.
        let config = Cli {
            platform_only: true,
            ..base_cli()
        };
        let v = resolve_qemu_version_from_cli(&config, true, no_docker).unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn resolve_qemu_version_direct_boot_requirement_bypassed_by_indirect_boot() {
        // direct_boot=false negates the guard's `direct_boot` term, so no error is raised even with no flag or distribution.
        let v = resolve_qemu_version_from_cli(&base_cli(), false, no_docker).unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn resolve_qemu_version_explicit_version_always_wins() {
        let config = Cli {
            qemu_version: Some("1:10.3.0-1ubuntu1".into()),
            distribution: Some("ubuntu:26.04".into()),
            ..base_cli()
        };
        let v = resolve_qemu_version_from_cli(&config, true, no_docker).unwrap();
        assert_eq!(v, Some("1:10.3.0-1ubuntu1".to_string()));

        // Still wins even without a distribution or direct boot.
        let config = Cli {
            qemu_version: Some("1.2.3".into()),
            ..base_cli()
        };
        let v = resolve_qemu_version_from_cli(&config, false, no_docker).unwrap();
        assert_eq!(v, Some("1.2.3".to_string()));
    }

    #[test]
    fn resolve_qemu_version_falls_back_to_distribution_default() {
        let expected = SUPPORTED_DISTRIBUTIONS
            .find("ubuntu:26.04")
            .unwrap()
            .qemu_default_version;
        let config = Cli {
            distribution: Some("ubuntu:26.04".into()),
            ..base_cli()
        };
        let v = resolve_qemu_version_from_cli(&config, false, no_docker).unwrap();
        assert_eq!(v, Some(expected.to_string()));
    }

    #[test]
    fn resolve_qemu_version_falls_back_to_none_for_unknown_distribution() {
        let config = Cli {
            distribution: Some("debian:12".into()),
            ..base_cli()
        };
        let v = resolve_qemu_version_from_cli(&config, false, no_docker).unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn resolve_qemu_version_empty_flag_skipped_outside_direct_boot_measurement() {
        let config = Cli {
            qemu_version: Some(String::new()),
            platform_only: true,
            ..base_cli()
        };
        let v = resolve_qemu_version_from_cli(&config, true, no_docker).unwrap();
        assert_eq!(v, None);

        let config = Cli {
            qemu_version: Some(String::new()),
            ..base_cli()
        };
        let v = resolve_qemu_version_from_cli(&config, false, no_docker).unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn platform_only_acpi_requires_archive_tip_configuration() {
        // Platform-only runs that generate ACPI tables must resolve the archive tip rather than return None, which would silently fall back to the distribution's pinned version.
        // Reaching the tip-resolution validation (instead of returning Ok(None)) proves the resolution path is taken, without needing Docker.
        let config = Cli {
            qemu_version: Some(String::new()),
            platform_only: true,
            create_acpi_tables: true,
            ..base_cli()
        };
        let err = resolve_qemu_version_from_cli(&config, true, no_docker).unwrap_err();
        assert!(err.to_string().contains("archive tip"));
    }

    #[test]
    fn resolve_qemu_version_empty_flag_requires_distribution_or_source_and_base_image() {
        let config = Cli {
            qemu_version: Some(String::new()),
            ..base_cli()
        };
        let err = resolve_qemu_version_from_cli(&config, true, no_docker).unwrap_err();
        assert!(err.to_string().contains("archive tip"));
    }

    #[test]
    fn resolve_qemu_version_empty_flag_without_distribution_requires_both_source_and_base_image() {
        let config = Cli {
            qemu_version: Some(String::new()),
            qemu_base_image: Some("ubuntu:26.04@sha256:abc".into()),
            ..base_cli()
        };
        let err = resolve_qemu_version_from_cli(&config, true, no_docker).unwrap_err();
        assert!(err.to_string().contains("--qemu-source"));

        let config = Cli {
            qemu_version: Some(String::new()),
            qemu_source: Some("main".into()),
            ..base_cli()
        };
        let err = resolve_qemu_version_from_cli(&config, true, no_docker).unwrap_err();
        assert!(err.to_string().contains("--qemu-base-image"));
    }

    #[test]
    fn platform_only_acpi_build_receives_resolved_archive_tip() {
        // A stubbed archive tip that differs from the distribution pin, so a silent fallback to the pin is detectable.
        const STUB_TIP: &str = "1:99.0.0+ds-0ubuntu1";
        let pinned = SUPPORTED_DISTRIBUTIONS
            .find("ubuntu:26.04")
            .unwrap()
            .qemu_default_version;
        assert_ne!(STUB_TIP, pinned);

        let config = Cli::parse_from([
            "tdx-measure",
            "metadata.json",
            "--platform-only",
            "--create-acpi-tables",
            "--distribution",
            "ubuntu:26.04",
            "--qemu-version",
            "",
        ]);

        let mut resolver_calls = 0;
        let resolved = resolve_qemu_version_from_cli(&config, true, |pkg| {
            resolver_calls += 1;
            assert_eq!(pkg.version, "", "tip resolution must not be pinned");
            Ok(STUB_TIP.to_string())
        })
        .unwrap();
        assert_eq!(resolver_calls, 1);
        assert_eq!(resolved.as_deref(), Some(STUB_TIP));

        let pkg = qemu_pkg_for_acpi_generation(&config, true, resolved.as_deref())
            .unwrap()
            .expect("ACPI build package missing for platform-only --create-acpi-tables");
        assert_eq!(pkg.version, STUB_TIP);
    }
}
