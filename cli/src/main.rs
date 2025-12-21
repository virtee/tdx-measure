/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use tdx_measure::{Machine, ImageConfig};
use fs_err as fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod transcript;
use transcript::generate_transcript;

const CREATE_ACPI_TABLES_SCRIPT: &str = include_str!("../../create_acpi_tables.sh");
const DOCKERFILE_QEMU_ACPI_DUMP: &str = include_str!("../../Dockerfile.qemu-acpi-dump");

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

    /// Generate ACPI tables for direct boot and a specific distribution, e.g., ubuntu:25.04
    #[arg(long)]
    create_acpi_tables: Option<String>,
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
    fn new(metadata_path: &Path, image_config: &ImageConfig, require_boot_config: bool) -> Result<Self> {
        let parent_dir = metadata_path.parent().unwrap_or(".".as_ref());

        // Handle optional boot_config
        let paths = if let Some(boot_config) = &image_config.boot_config {
            PathStorage {
                cpu_count: boot_config.cpus,
                memory_size: image_config.memory_size()?,
                firmware: parent_dir.join(&boot_config.bios).display().to_string(),
                cmdline: image_config.cmdline().to_string(),
                acpi_tables: parent_dir.join(&boot_config.acpi_tables).display().to_string(),
                rsdp: boot_config.rsdp.as_ref().map(|p| parent_dir.join(p).display().to_string()),
                table_loader: boot_config.table_loader.as_ref().map(|p| parent_dir.join(p).display().to_string()),
                boot_order: boot_config.boot_order.as_ref().map(|p| parent_dir.join(p).display().to_string()),
                path_boot_xxxx: boot_config.path_boot_xxxx.as_ref().map(|p| parent_dir.join(p).display().to_string()),
                kernel: image_config.direct_boot().map(|d| parent_dir.join(&d.kernel).display().to_string()),
                initrd: image_config.direct_boot().map(|d| parent_dir.join(&d.initrd).display().to_string()),
                qcow2: image_config.indirect_boot().map(|i| parent_dir.join(&i.qcow2).display().to_string()),
                mok_list: image_config.indirect_boot().map(|i| parent_dir.join(&i.mok_list).display().to_string()),
                mok_list_trusted: image_config.indirect_boot().map(|i| parent_dir.join(&i.mok_list_trusted).display().to_string()),
                mok_list_x: image_config.indirect_boot().map(|i| parent_dir.join(&i.mok_list_x).display().to_string()),
                sbat_level: image_config.indirect_boot().map(|i| parent_dir.join(&i.sbat_level).display().to_string()),
            }
        } else {
            // When boot_config is None (runtime-only mode), provide empty strings for platform fields
            if require_boot_config {
                return Err(anyhow!("Boot info is required but not provided in the configuration"));
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
                kernel: image_config.direct_boot().map(|d| parent_dir.join(&d.kernel).display().to_string()),
                initrd: image_config.direct_boot().map(|d| parent_dir.join(&d.initrd).display().to_string()),
                qcow2: image_config.indirect_boot().map(|i| parent_dir.join(&i.qcow2).display().to_string()),
                mok_list: image_config.indirect_boot().map(|i| parent_dir.join(&i.mok_list).display().to_string()),
                mok_list_trusted: image_config.indirect_boot().map(|i| parent_dir.join(&i.mok_list_trusted).display().to_string()),
                mok_list_x: image_config.indirect_boot().map(|i| parent_dir.join(&i.mok_list_x).display().to_string()),
                sbat_level: image_config.indirect_boot().map(|i| parent_dir.join(&i.sbat_level).display().to_string()),
            }
        };

        Ok(Self { paths })
    }

    fn build_machine(&self, direct_boot: bool) -> Machine<'_> {
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
            .kernel(self.paths.kernel.as_deref().unwrap_or(""))
            .initrd(self.paths.initrd.as_deref().unwrap_or(""))
            .qcow2(self.paths.qcow2.as_deref().unwrap_or(""))
            .mok_list(self.paths.mok_list.as_deref().unwrap_or(""))
            .mok_list_trusted(self.paths.mok_list_trusted.as_deref().unwrap_or(""))
            .mok_list_x(self.paths.mok_list_x.as_deref().unwrap_or(""))
            .sbat_level(self.paths.sbat_level.as_deref().unwrap_or(""))
            .direct_boot(direct_boot)
            .build()
    }
}

fn generate_acpi_tables(metadata_path: &Path, distribution: &str) -> Result<()> {
    let tmp_dir = std::env::temp_dir();

    // Write the embedded script to a temporary file
    let script_path = tmp_dir.join("create_acpi_tables.sh");
    std::fs::write(&script_path, CREATE_ACPI_TABLES_SCRIPT)
        .context("Failed to write create_acpi_tables.sh to temporary directory")?;

    // Write the embedded Dockerfile to a temporary file
    let dockerfile_path = tmp_dir.join("Dockerfile.qemu-acpi-dump");
    std::fs::write(&dockerfile_path, DOCKERFILE_QEMU_ACPI_DUMP)
        .context("Failed to write Dockerfile.qemu-acpi-dump to temporary directory")?;

    // Call dedicated bash script to create ACPI tables.
    // TODO: Integrate functionality from bash script into `acpi.rs`.
    let output = Command::new("bash")
        .arg(&script_path)
        .arg("-j")
        .arg(metadata_path)
        .arg("-d")
        .arg(distribution)
        .output()
        .context("Failed to execute create_acpi_tables.sh script")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("ACPI table generation failed: {}", stderr));
    }
    Ok(())
}

fn process_measurements(config: &Cli, image_config: &ImageConfig) -> Result<()> {
    // Validate the configuration
    image_config.validate()
        .map_err(|e| anyhow!("Invalid image configuration: {}", e))?;

    let mut direct_boot = true; // Direct boot by default
    if !config.platform_only { // If platform only, skip boot mode validation
        // Determine boot mode: CLI flag overrides JSON configuration
        direct_boot = config.direct_boot.unwrap_or(image_config.is_direct_boot());

        // Validate boot mode configuration
        match (direct_boot, image_config.direct_boot(), image_config.indirect_boot()) {
            (true, None, _) => return Err(anyhow!("Direct boot mode specified but no direct boot configuration found in JSON")),
            (false, _, None) => return Err(anyhow!("Indirect boot mode specified but no indirect boot configuration found in JSON")),
            _ => {}
        }
    }

    // Generate ACPI tables if requested
    if let Some(ref distribution) = config.create_acpi_tables {
        if !direct_boot {
            return Err(anyhow!("--create-acpi-tables flag is only valid with direct boot mode"));
        }

        generate_acpi_tables(&config.metadata, distribution)?;
    }

    // Build machine
    let path_resolver = PathResolver::new(&config.metadata, image_config, !config.runtime_only)?;
    let machine = path_resolver.build_machine(direct_boot);

    // Generate transcript
    if let Some(ref transcript_file) = config.transcript {
        return generate_transcript(transcript_file, &path_resolver, direct_boot, config.platform_only, config.runtime_only);
    }

    // Measure
    let measurements = if config.platform_only {
        machine.measure_platform().context("Failed to measure platform")?
    } else if config.runtime_only {
        machine.measure_runtime().context("Failed to measure runtime")?
    } else {
        machine.measure().context("Failed to measure machine configuration")?
    };

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
        fs::write(json_file, json_output)
            .context("Failed to write measurements to file")?;
    }

    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let metadata = fs::read_to_string(&cli.metadata)
        .context("Failed to read image metadata")?;
    let image_config: ImageConfig = serde_json::from_str(&metadata)
        .context("Failed to parse image metadata")?;

    process_measurements(&cli, &image_config)?;

    Ok(())
}
