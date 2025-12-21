/*
 * Copyright (c) 2025 Phala Network
 * Copyright (c) 2025 Tinfoil Inc
 * SPDX-License-Identifier: Apache-2.0
 */
use crate::{measure_log, measure_sha384, util::debug_print_log, util::authenticode_sha384_hash};
use anyhow::{bail, Context, Result};
use fs_err as fs;

/// Patches the kernel image as qemu does.
fn patch_kernel(
    kernel_data: &[u8],
    initrd_size: u32,
    mem_size: u64,
    acpi_data_size: u32,
) -> Result<Vec<u8>> {
    const MIN_KERNEL_LENGTH: usize = 0x1000;
    if kernel_data.len() < MIN_KERNEL_LENGTH {
        bail!("the kernel image is too short");
    }

    let mut kd = kernel_data.to_vec();

    let protocol = u16::from_le_bytes(kd[0x206..0x208].try_into().unwrap());

    let (real_addr, cmdline_addr) = if protocol < 0x200 || (kd[0x211] & 0x01) == 0 {
        (0x90000_u32, 0x9a000_u32)
    } else {
        (0x10000_u32, 0x20000_u32)
    };

    if protocol >= 0x200 {
        kd[0x210] = 0xb0; // type_of_loader = Qemu v0
    }
    if protocol >= 0x201 {
        kd[0x211] |= 0x80; // loadflags |= CAN_USE_HEAP
        let heap_end_ptr = cmdline_addr - real_addr - 0x200;
        kd[0x224..0x228].copy_from_slice(&heap_end_ptr.to_le_bytes());
    }
    if protocol >= 0x202 {
        kd[0x228..0x22C].copy_from_slice(&cmdline_addr.to_le_bytes());
    } else {
        kd[0x20..0x22].copy_from_slice(&0xa33f_u16.to_le_bytes());
        let offset = (cmdline_addr - real_addr) as u16;
        kd[0x22..0x24].copy_from_slice(&offset.to_le_bytes());
    }

    if initrd_size > 0 {
        if protocol < 0x200 {
            bail!("the kernel image is too old for ramdisk");
        }
        let mut initrd_max = if protocol >= 0x20c {
            let xlf = u16::from_le_bytes(kd[0x236..0x238].try_into().unwrap());
            if (xlf & 0x40) != 0 {
                u32::MAX
            } else {
                0x37ffffff
            }
        } else if protocol >= 0x203 {
            let max = u32::from_le_bytes(kd[0x22c..0x230].try_into().unwrap());
            if max == 0 {
                0x37ffffff
            } else {
                max
            }
        } else {
            0x37ffffff
        };

        let lowmem = if mem_size < 0xb0000000 {
            0xb0000000
        } else {
            0x80000000
        };
        let below_4g_mem_size = if mem_size >= lowmem {
            lowmem as u32
        } else {
            mem_size as u32
        };

        if initrd_max >= below_4g_mem_size - acpi_data_size {
            initrd_max = below_4g_mem_size - acpi_data_size - 1;
        }
        if initrd_size >= initrd_max {
            bail!("initrd is too large");
        }

        let initrd_addr = (initrd_max - initrd_size) & !4095;
        kd[0x218..0x21C].copy_from_slice(&initrd_addr.to_le_bytes());
        kd[0x21C..0x220].copy_from_slice(&initrd_size.to_le_bytes());
    }
    Ok(kd)
}

/// Measures a QEMU-patched TDX kernel image from file paths (for direct boot).
pub(crate) fn measure_rtmr1_direct(
    kernel_path: &str,
    initrd_path: &str,
    mem_size: u64,
    acpi_data_size: u32,
) -> Result<Vec<u8>> {

    // Read kernel and initrd files
    let kernel_data = fs::read(kernel_path).context("Failed to read kernel file")?;
    let initrd_data = fs::read(initrd_path).context("Failed to read initrd file")?;
    let initrd_size = initrd_data.len() as u32;

    // Patch kernel to mimic QEMU's behavior
    let kd = patch_kernel(&kernel_data, initrd_size, mem_size, acpi_data_size)
        .context("Failed to patch kernel")?;
    let kernel_hash = authenticode_sha384_hash(&kd).context("Failed to compute kernel hash")?;

    // Compute RTMR1 log
    let rtmr1_log = vec![
        kernel_hash,
        measure_sha384(b"Calling EFI Application from Boot Option"),
        measure_sha384(&[0x00, 0x00, 0x00, 0x00]), // Separator
        measure_sha384(b"Exit Boot Services Invocation"),
        measure_sha384(b"Exit Boot Services Returned with Success"),
    ];

    debug_print_log("RTMR1", &rtmr1_log);
    Ok(measure_log(&rtmr1_log))
}

/// Measures RTMR2 for direct boot from file paths.
pub(crate) fn measure_rtmr2_direct(
    initrd_path: &str,
    kernel_cmdline: &str,
) -> Result<Vec<u8>> {

    // Reads our initrd file
    let initrd_data = fs::read(initrd_path).context("Failed to read initrd file")?;

    // OVFM adds initrd to the command line
    let cmdline = kernel_cmdline.to_string() + " initrd=initrd";

    // Compute RTMR2 log
    let rtmr2_log = vec![
        crate::util::measure_cmdline(&cmdline),
        measure_sha384(&initrd_data),
    ];

    debug_print_log("RTMR2", &rtmr2_log);
    Ok(measure_log(&rtmr2_log))
}
