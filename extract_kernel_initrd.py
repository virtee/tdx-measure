#!/usr/bin/env python3
# Copyright (c) 2026 Intel Corporation
# SPDX-License-Identifier: Apache-2.0

"""Extract kernel and initrd from a TD (Trust Domain) disk image.

Requires: python3-apt, qemu-utils, and root privileges.
Install with: sudo apt install python3-apt qemu-utils
"""

from __future__ import annotations

import argparse
import glob
import os
import shutil
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from functools import cmp_to_key
from pathlib import Path

try:
    import apt_pkg  # pyright: ignore[reportMissingImports]
except ModuleNotFoundError:
    sys.exit(
        "Error: python3-apt is not installed.\n"
        "Install it with: sudo apt install python3-apt"
    )


apt_pkg.init()

# The fallback poll for partition device is a heuristic backstop for when udevadm is unavailable or returns before the nodes appear.
# The fallback runs at most the number of times defined here and waits before each attempt the amount of seconds defined here.
PARTITION_POLL_ATTEMPTS = 20
PARTITION_POLL_INTERVAL_SECONDS = 0.1

def boot_version(filename: str) -> str:
    """Return the Debian version portion of a boot artifact filename."""
    for prefix in ("vmlinuz-", "initrd.img-"):
        if filename.startswith(prefix):
            return filename.removeprefix(prefix)
    return filename


def compare_boot_versions(left: str, right: str) -> int:
    """Compare boot artifact filenames using Debian version semantics."""
    return apt_pkg.version_compare(boot_version(left), boot_version(right))


def run(command: list[str]) -> None:
    """Run a command and turn failures into a concise user-facing error."""
    try:
        subprocess.run(command, check=True)
    except subprocess.CalledProcessError as exc:
        sys.exit(f"Error: Command failed: {' '.join(command)}\n{exc}")


def ensure_nbd_module_loaded() -> None:
    """Exit with guidance when the nbd kernel module is not loaded."""
    if Path("/sys/module/nbd").exists():
        return
    sys.exit(
        "Error: The nbd kernel module is not loaded.\n"
        "Load it with the following command, then rerun this script:\n"
        "  sudo modprobe nbd\n"
        "To load it automatically after reboot, create '/etc/modules-load.d/nbd.conf' with the following content:\n"
        "  nbd"
    )


def find_unused_nbd_device() -> Path:
    """Return an available NBD device, or exit when no device is available."""
    ensure_nbd_module_loaded()

    # NBD availability reporting is kernel-version dependent, so use a layered strategy.
    # Strategy 1: Check /sys/block/nbdX/pid presence because on modern kernels the file is only written while a connection is active.
    # Strategy 2: Fall back to /sys/block/nbdX/size because size is a universal block-device attribute and reads as 0 when disconnected.
    # The first device that passes these checks is returned and qemu-nbd performs the final exclusivity check.
    for device in map(Path, sorted(glob.glob("/dev/nbd[0-9]*"))):
        # The glob also matches partition nodes such as /dev/nbd0p1, so filter to base devices only.
        if not device.name.removeprefix("nbd").isdigit():
            continue

        # A matching name is not sufficient when the device node is stale or absent.
        if not device.is_block_device():
            continue

        # On modern kernels the pid file exists only while a connection is active, so its presence alone means busy.
        pid_path = Path("/sys/block") / device.name / "pid"
        if pid_path.exists():
            continue

        # size is present for every registered block device, so an unreadable value indicates an unexpected sysfs state; skip the device.
        size_path = Path("/sys/block") / device.name / "size"
        try:
            if int(size_path.read_text().strip()) == 0:
                return device
        except (OSError, ValueError):
            pass
    sys.exit(
        "Error: No unused /dev/nbdN device found.\n"
        "This usually means all NBD devices are in use, or device nodes are unavailable.\n"
        "Disconnect existing qemu-nbd mappings, then retry."
    )


def nbd_partition_capacity() -> int | None:
    """Return the active NBD partition limit when the module exposes one."""
    try:
        return int(Path("/sys/module/nbd/parameters/max_part").read_text().strip())
    except (OSError, ValueError):
        return None


def image_partition_numbers(nbd_device: Path) -> list[int]:
    """Read partition numbers directly from the connected image partition table."""
    if shutil.which("partx") is None:
        return []
    try:
        result = subprocess.run(
            ["partx", "--show", "--raw", "--noheadings", "--output", "NR", str(nbd_device)],
            check=True,
            capture_output=True,
            text=True,
        )
    except subprocess.CalledProcessError:
        return []
    return [int(number) for number in result.stdout.split()]


def validate_nbd_partition_capacity(nbd_device: Path) -> None:
    """Exit with a module reload command when the image has too many partitions."""
    partition_limit = nbd_partition_capacity()
    partition_numbers = image_partition_numbers(nbd_device)
    if partition_limit is None or not partition_numbers:
        return
    required_partition = max(partition_numbers)
    if required_partition <= partition_limit:
        return
    requested_capacity = max(required_partition + 1, 16)
    sys.exit(
        f"Error: The image uses partition {required_partition}, but the loaded nbd module supports only {partition_limit} partitions per device.\n"
        "After this script disconnects the image, reload the module with:\n"
        "  sudo modprobe -r nbd\n"
        f"  sudo modprobe nbd max_part={requested_capacity}"
    )


def partition_paths(nbd_device: Path) -> list[Path]:
    """Return the partitions exposed by an NBD device after qemu-nbd connects it."""
    # If available, wait for udev to process partition events from qemu-nbd.
    # Some systems still expose /dev/nbdNp* after settle returns, so keep a bounded poll as a backstop.
    # If udevadm is unavailable, the poll below is the only partition-discovery path.
    if shutil.which("udevadm") is not None:
        subprocess.run(["udevadm", "settle"], check=False)

    # Poll for partition block devices until they appear or the retry budget is exhausted.
    for _ in range(PARTITION_POLL_ATTEMPTS):
        partitions = sorted(
            Path(path)
            for path in glob.glob(f"{nbd_device}p*")
            if Path(path).is_block_device()
        )
        if partitions:
            return partitions
        time.sleep(PARTITION_POLL_INTERVAL_SECONDS)
    sys.exit(f"Error: No partitions appeared on '{nbd_device}'.")


@contextmanager
def mounted_boot_directory(image: Path):
    """Connect an image read-only and yield the partition containing /boot."""

    # Find an unused NBD device.
    nbd_device = find_unused_nbd_device()

    # Create a temporary directory to mount the partition.
    mount_dir = Path(tempfile.mkdtemp(prefix="tdx-boot-"))

    partition_mounted = False
    device_connected = False
    try:
        # Connect the image to the NBD device read-only.
        run(["qemu-nbd", "--read-only", "--connect", str(nbd_device), str(image)])
        device_connected = True

        # Validate the NBD device's partition capacity.
        validate_nbd_partition_capacity(nbd_device)

        # Iterate over the partitions and attempt to mount each one read-only.
        for partition in partition_paths(nbd_device):
            # A partition that cannot be mounted is simply not the one we are looking for.
            result = subprocess.run(
                ["mount", "--read-only", str(partition), str(mount_dir)], check=False
            )
            if result.returncode != 0:
                continue
            partition_mounted = True

            # Images may have a root filesystem with /boot or a dedicated boot partition.
            boot_dir = mount_dir / "boot" if (mount_dir / "boot").is_dir() else mount_dir
            if any(boot_dir.glob("vmlinuz-*")):
                yield boot_dir
                return

            run(["umount", str(mount_dir)])
            partition_mounted = False
        sys.exit(f"Error: No partition containing /boot was found in '{image}'.")
    finally:
        if partition_mounted:
            subprocess.run(["umount", str(mount_dir)], check=False)
        if device_connected:
            # The qemu-nbd output is suppressed so this block is the only statement about the disconnect outcome.
            disconnect = subprocess.run(
                ["qemu-nbd", "--disconnect", str(nbd_device)],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            if disconnect.returncode == 0:
                print(f"Disconnected the image from '{nbd_device}'.")
            else:
                # A stale connection blocks later runs, so the user needs to know the device was left connected.
                print(
                    f"Warning: Failed to disconnect the image from '{nbd_device}'.\n"
                    f"Disconnect it manually with: qemu-nbd --disconnect {nbd_device}",
                    file=sys.stderr,
                )
        shutil.rmtree(mount_dir, ignore_errors=True)


def list_boot_files(boot_dir: Path) -> list[str]:
    """List filenames in a mounted guest /boot directory."""
    try:
        return sorted(path.name for path in boot_dir.iterdir())
    except OSError as exc:
        sys.exit(f"Error: Failed to list '{boot_dir}'.\n{exc}")


def restore_output_ownership(paths: tuple[Path, Path]) -> None:
    """Give extracted files to the user who invoked sudo, when applicable."""
    # sudo preserves the original caller's IDs so root does not leave inaccessible output.
    sudo_uid = os.environ.get("SUDO_UID")
    sudo_gid = os.environ.get("SUDO_GID")
    if sudo_uid is None or sudo_gid is None:
        return
    try:
        uid = int(sudo_uid)
        gid = int(sudo_gid)
        for path in paths:
            os.chown(path, uid, gid)
    except ValueError as exc:
        raise OSError("Invalid sudo user or group ID") from exc


def select_kernel_and_initrd(
    boot_files: list[str], requested_version: str | None
) -> tuple[str, str]:
    """Select kernel and initrd filenames from the boot listing."""
    kernels = sorted(
        [f for f in boot_files if f.startswith("vmlinuz-")],
        key=cmp_to_key(compare_boot_versions),
    )
    initrds = sorted(
        [f for f in boot_files if f.startswith("initrd.img-")],
        key=cmp_to_key(compare_boot_versions),
    )

    if not kernels:
        sys.exit("Error: No kernel (vmlinuz-*) found in /boot/")
    if not initrds:
        sys.exit("Error: No initrd (initrd.img-*) found in /boot/")

    print("\nAvailable kernels:")
    for i, k in enumerate(kernels, 1):
        print(f"  {i}\t{k}")
    print("\nAvailable initrds:")
    for i, r in enumerate(initrds, 1):
        print(f"  {i}\t{r}")
    print()

    if requested_version:
        print(f"Requested kernel version: {requested_version}")
        kernel_file = f"vmlinuz-{requested_version}"
        initrd_file = f"initrd.img-{requested_version}"

        if kernel_file not in kernels:
            available = ", ".join(k.removeprefix("vmlinuz-") for k in kernels)
            sys.exit(
                f"Error: Kernel '{kernel_file}' not found in image.\n"
                f"Available versions: {available}"
            )
        if initrd_file not in initrds:
            sys.exit(f"Error: Initrd '{initrd_file}' not found in image.")

        return kernel_file, initrd_file

    # Auto-select: latest kernel with its exactly-matching initrd
    kernel_file = kernels[-1]
    version = kernel_file.removeprefix("vmlinuz-")
    matching_initrd = f"initrd.img-{version}"

    if matching_initrd not in initrds:
        available = ", ".join(r.removeprefix("initrd.img-") for r in initrds)
        sys.exit(
            f"Error: No initrd matching kernel version '{version}'.\n"
            f"A kernel and initrd must be from the same version. "
            f"Available initrd versions: {available}\n"
            f"Pass --kernel-version to pick a version that has both."
        )

    return kernel_file, matching_initrd


def extract_files(
    boot_dir: Path,
    kernel_file: str,
    initrd_file: str,
    output_dir: Path,
) -> tuple[Path, Path]:
    """Copy the selected kernel and initrd from a mounted guest filesystem."""
    # Filenames come from the untrusted disk image listing, so reject paths that escape output_dir.
    out_resolved = output_dir.resolve()
    for name in (kernel_file, initrd_file):
        try:
            (output_dir / name).resolve().relative_to(out_resolved)
        except ValueError:
            sys.exit(f"Error: Refusing unsafe filename from image: '{name}'")

    kernel_dst = output_dir / kernel_file
    initrd_dst = output_dir / initrd_file

    print(f"Extracting kernel: {kernel_file}")
    print(f"Extracting initrd: {initrd_file}")

    # Copy files and restore ownership to the original user if run with sudo.
    try:
        shutil.copyfile(boot_dir / kernel_file, kernel_dst)
        shutil.copyfile(boot_dir / initrd_file, initrd_dst)
        restore_output_ownership((kernel_dst, initrd_dst))
    except OSError as exc:
        # Clean up partial files on failure (best-effort)
        for partial in (kernel_dst, initrd_dst):
            try:
                partial.unlink(missing_ok=True)
            except OSError as cleanup_exc:
                # Report the leftover file without masking the original extraction error.
                print(
                    f"Warning: Failed to remove partial file '{partial}': {cleanup_exc}",
                    file=sys.stderr,
                )
        sys.exit(f"Error: Extraction failed.\n{exc}")

    return kernel_dst, initrd_dst


def validate_output(kernel_dst: Path, initrd_dst: Path) -> None:
    """Verify extracted files are non-empty."""
    for path in (kernel_dst, initrd_dst):
        if not path.exists() or path.stat().st_size == 0:
            sys.exit(f"Error: Extracted file '{path}' is empty or missing.")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Extract kernel and initrd from a TD (Trust Domain) disk image."
    )
    parser.add_argument("image", help="Path to the TD disk image (qcow2, raw, etc.)")
    parser.add_argument(
        "output_dir",
        nargs="?",
        default=".",
        help="Output directory (default: current directory)",
    )
    parser.add_argument(
        "--kernel-version",
        dest="kernel_version",
        default=None,
        help="Select a specific kernel version (e.g. 7.0.0-14-generic)",
    )
    args = parser.parse_args()

    image = Path(args.image)
    output_dir = Path(args.output_dir)

    if not image.is_file():
        sys.exit(f"Error: Image file '{image}' not found.")

    # Check for root privileges and required tools.
    if os.geteuid() != 0:
        script_name = Path(sys.argv[0]).name
        sys.exit(
            "Error: Run this script as root because qemu-nbd and mount require it.\n"
            f"Try: sudo python3 {script_name} ..."
        )
    if shutil.which("qemu-nbd") is None:
        sys.exit(
            "Error: qemu-nbd is not installed.\n"
            "Install it with: sudo apt install qemu-utils"
        )

    print(f"Inspecting image: {image}")
    print("Finding kernel and initrd files in /boot...")

    # Mount the image and extract the kernel and initrd files.
    with mounted_boot_directory(image) as boot_dir:
        # List the files in the mounted /boot directory and select the kernel and initrd.
        boot_files = list_boot_files(boot_dir)
        kernel_file, initrd_file = select_kernel_and_initrd(
            boot_files, args.kernel_version
        )

        # Create the output directory if it doesn't exist, and ensure it's a directory.
        if output_dir.exists() and not output_dir.is_dir():
            sys.exit(
                f"Error: Output path '{output_dir}' exists and is not a directory."
            )
        try:
            output_dir.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            sys.exit(
                f"Error: Failed to create output directory '{output_dir}': {exc}"
            )

        # Extract the selected kernel and initrd files to the output directory.
        kernel_dst, initrd_dst = extract_files(
            boot_dir, kernel_file, initrd_file, output_dir
        )

    validate_output(kernel_dst, initrd_dst)

    print("\nDone. Extracted files:")
    for path in (kernel_dst, initrd_dst):
        size_mb = path.stat().st_size / (1024 * 1024)
        print(f"  {path}  ({size_mb:.1f} MB)")


if __name__ == "__main__":
    main()
