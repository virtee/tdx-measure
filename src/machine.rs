/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * Copyright (c) 2025-2026 Intel Corporation
 * SPDX-License-Identifier: Apache-2.0
 */
use crate::docker::QemuPkg;
use crate::tdvf::Tdvf;
use crate::{image, kernel, TdxMeasurements};
use anyhow::{Context, Result};
use fs_err as fs;
use log::debug;
use std::path::Path;

#[derive(Debug, bon::Builder)]
pub struct Machine<'a> {
    pub cpu_count: u8,
    pub memory_size: u64,
    pub qcow2: Option<&'a str>,
    pub firmware: &'a str,
    pub kernel: Option<&'a str>,
    pub initrd: Option<&'a str>,
    pub kernel_cmdline: &'a str,
    pub acpi_tables: &'a str,
    pub rsdp: &'a str,
    pub table_loader: &'a str,
    pub boot_order: &'a str,
    pub path_boot_xxxx: &'a str,
    pub smbios: Option<&'a str>,
    pub mok_list: Option<&'a str>,
    pub mok_list_trusted: Option<&'a str>,
    pub mok_list_x: Option<&'a str>,
    pub sbat_level: Option<&'a str>,
    pub direct_boot: bool,
    pub metadata_path: &'a Path,
    pub create_acpi_table: bool,
    pub qemu_major: Option<u32>,
    pub qemu_pkg: Option<QemuPkg<'a>>,
    pub ovmf_version: Option<(u16, u8)>,
}

impl Machine<'_> {
    pub fn measure(&self) -> Result<TdxMeasurements> {
        debug!("measuring machine: {self:#?}");

        // Measure platform
        let fw_data = fs::read(self.firmware)?;
        let tdvf = Tdvf::parse(&fw_data).context("Failed to parse TDVF metadata")?;
        let mrtd = tdvf.mrtd().context("Failed to compute MR TD")?;
        let rtmr0 = tdvf.rtmr0(self).context("Failed to compute RTMR0")?;

        let rtmr1;
        let rtmr2;

        // Direct boot
        if self.direct_boot {
            let kernel_path = self
                .kernel
                .ok_or_else(|| anyhow::anyhow!("Kernel path required for direct boot"))?;
            let initrd_path = self
                .initrd
                .ok_or_else(|| anyhow::anyhow!("Initrd path required for direct boot"))?;

            let ovmf_version = self.ovmf_version.ok_or_else(|| {
                anyhow::anyhow!("OVMF version required for direct boot RTMR2 measurement")
            })?;

            let qemu_major = self.qemu_major.ok_or_else(|| {
                anyhow::anyhow!("QEMU major version required for direct boot RTMR1 measurement")
            })?;
            rtmr1 = kernel::measure_rtmr1_direct(
                kernel_path,
                initrd_path,
                self.memory_size,
                0x28000,
                qemu_major,
            )?;
            rtmr2 = kernel::measure_rtmr2_direct(initrd_path, self.kernel_cmdline, ovmf_version)?;
        } else {
            // Indirect boot
            let qcow2_path = self
                .qcow2
                .ok_or_else(|| anyhow::anyhow!("Qcow2 path required for indirect boot"))?;
            let mok_list_path = self
                .mok_list
                .ok_or_else(|| anyhow::anyhow!("MOK list path required for indirect boot"))?;
            let mok_list_trusted_path = self.mok_list_trusted.ok_or_else(|| {
                anyhow::anyhow!("MOK list trusted path required for indirect boot")
            })?;
            let mok_list_x_path = self
                .mok_list_x
                .ok_or_else(|| anyhow::anyhow!("MOK list X path required for indirect boot"))?;

            rtmr1 = image::measure_rtmr1_from_qcow2(qcow2_path)?;
            rtmr2 = image::measure_rtmr2_from_qcow2(
                qcow2_path,
                self.kernel_cmdline,
                mok_list_path,
                mok_list_trusted_path,
                mok_list_x_path,
            )?;
        }

        Ok(TdxMeasurements {
            mrtd,
            rtmr0,
            rtmr1,
            rtmr2,
        })
    }

    pub fn measure_platform(&self) -> Result<TdxMeasurements> {
        let fw_data = fs::read(self.firmware)?;
        let tdvf = Tdvf::parse(&fw_data).context("Failed to parse TDVF metadata")?;
        let mrtd = tdvf.mrtd().context("Failed to compute MR TD")?;
        let rtmr0 = tdvf.rtmr0(self).context("Failed to compute RTMR0")?;

        Ok(TdxMeasurements {
            mrtd,
            rtmr0,
            rtmr1: vec![],
            rtmr2: vec![],
        })
    }

    pub fn measure_runtime(&self) -> Result<TdxMeasurements> {
        let rtmr1;
        let rtmr2;

        // Direct boot
        if self.direct_boot {
            let kernel_path = self
                .kernel
                .ok_or_else(|| anyhow::anyhow!("Kernel path required for direct boot"))?;
            let initrd_path = self
                .initrd
                .ok_or_else(|| anyhow::anyhow!("Initrd path required for direct boot"))?;

            let ovmf_version = self.ovmf_version.ok_or_else(|| {
                anyhow::anyhow!("OVMF version required for direct boot RTMR2 measurement")
            })?;

            let qemu_major = self.qemu_major.ok_or_else(|| {
                anyhow::anyhow!("QEMU major version required for direct boot RTMR1 measurement")
            })?;
            // WARN : Carefull, when measuring the runtime only, we only compute the measurement for memory size > 0xb0000000
            rtmr1 = kernel::measure_rtmr1_direct(
                kernel_path,
                initrd_path,
                0xb0000000,
                0x28000,
                qemu_major,
            )?;
            rtmr2 = kernel::measure_rtmr2_direct(initrd_path, self.kernel_cmdline, ovmf_version)?;
        } else {
            // Indirect boot
            let qcow2_path = self
                .qcow2
                .ok_or_else(|| anyhow::anyhow!("Qcow2 path required for indirect boot"))?;
            let mok_list_path = self
                .mok_list
                .ok_or_else(|| anyhow::anyhow!("MOK list path required for indirect boot"))?;
            let mok_list_trusted_path = self.mok_list_trusted.ok_or_else(|| {
                anyhow::anyhow!("MOK list trusted path required for indirect boot")
            })?;
            let mok_list_x_path = self
                .mok_list_x
                .ok_or_else(|| anyhow::anyhow!("MOK list X path required for indirect boot"))?;

            rtmr1 = image::measure_rtmr1_from_qcow2(qcow2_path)?;
            rtmr2 = image::measure_rtmr2_from_qcow2(
                qcow2_path,
                self.kernel_cmdline,
                mok_list_path,
                mok_list_trusted_path,
                mok_list_x_path,
            )?;
        }

        Ok(TdxMeasurements {
            mrtd: vec![],
            rtmr0: vec![],
            rtmr1,
            rtmr2,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A direct-boot `Machine` with no `firmware`/`acpi_tables` fixtures, since `measure_runtime` never reads them.
    fn direct_boot_machine(qemu_major: Option<u32>) -> Machine<'static> {
        Machine::builder()
            .cpu_count(1)
            .memory_size(0)
            .firmware("")
            .kernel_cmdline("")
            .acpi_tables("")
            .rsdp("")
            .table_loader("")
            .boot_order("")
            .path_boot_xxxx("")
            .direct_boot(true)
            .metadata_path(Path::new(""))
            .create_acpi_table(false)
            .kernel("kernel")
            .initrd("initrd")
            .ovmf_version((2025, 5))
            .maybe_qemu_major(qemu_major)
            .build()
    }

    #[test]
    fn measure_runtime_errors_without_qemu_major() {
        let err = direct_boot_machine(None).measure_runtime().unwrap_err();
        assert!(
            err.to_string()
                .contains("QEMU major version required for direct boot RTMR1 measurement"),
            "unexpected error: {err}",
        );
    }
}
