#!/usr/bin/env python3
# Copyright (c) 2026 Intel Corporation
# SPDX-License-Identifier: Apache-2.0

"""Unit tests for extract_kernel_initrd.py.

Requirements:
    sudo apt install python3-apt python3-pytest

Run from the repository root:
    pytest tests/test_extract_kernel_initrd.py

These tests are self-contained and do not require qemu-utils, root access, or a real disk image.
NBD commands are faked or monkeypatched.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

# Import the module under test
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import extract_kernel_initrd as eki


# ---------------------------------------------------------------------------
# Debian version comparison
# ---------------------------------------------------------------------------

class TestBootVersionComparison:
    def test_removes_boot_artifact_prefix(self):
        assert eki.boot_version("vmlinuz-6.12.0-14-generic") == "6.12.0-14-generic"
        assert eki.boot_version("initrd.img-6.12.0-14-generic") == "6.12.0-14-generic"

    def test_orders_versions_using_debian_semantics(self):
        names = [
            "vmlinuz-6.8.0-10-generic",
            "vmlinuz-6.12.0-14-generic",
            "vmlinuz-6.12.0-2-generic",
            "vmlinuz-5.15.0-100-generic",
        ]
        sorted_names = sorted(names, key=eki.cmp_to_key(eki.compare_boot_versions))
        assert sorted_names == [
            "vmlinuz-5.15.0-100-generic",
            "vmlinuz-6.8.0-10-generic",
            "vmlinuz-6.12.0-2-generic",
            "vmlinuz-6.12.0-14-generic",
        ]

    def test_prerelease_sorts_before_final_release(self):
        assert (
            eki.compare_boot_versions("vmlinuz-6.8.0~rc1", "vmlinuz-6.8.0")
            < 0
        )

    def test_epoch_sorts_after_version_without_an_epoch(self):
        assert eki.compare_boot_versions("vmlinuz-1:6.8.0", "vmlinuz-6.8.0") > 0


# ---------------------------------------------------------------------------
# select_kernel_and_initrd
# ---------------------------------------------------------------------------

BOOT_FILES = [
    "vmlinuz-6.8.0-10-generic",
    "vmlinuz-6.12.0-14-generic",
    "initrd.img-6.8.0-10-generic",
    "initrd.img-6.12.0-14-generic",
    "config-6.12.0-14-generic",
    "System.map-6.12.0-14-generic",
]


class TestSelectKernelAndInitrd:
    def test_auto_selects_latest_pair(self):
        kernel, initrd = eki.select_kernel_and_initrd(BOOT_FILES, None)
        assert kernel == "vmlinuz-6.12.0-14-generic"
        assert initrd == "initrd.img-6.12.0-14-generic"

    def test_auto_selects_final_release_over_prerelease(self):
        files = [
            "vmlinuz-6.8.0~rc1",
            "vmlinuz-6.8.0",
            "initrd.img-6.8.0~rc1",
            "initrd.img-6.8.0",
        ]
        kernel, initrd = eki.select_kernel_and_initrd(files, None)
        assert kernel == "vmlinuz-6.8.0"
        assert initrd == "initrd.img-6.8.0"

    def test_explicit_version_selects_pinned_pair(self):
        kernel, initrd = eki.select_kernel_and_initrd(BOOT_FILES, "6.8.0-10-generic")
        assert kernel == "vmlinuz-6.8.0-10-generic"
        assert initrd == "initrd.img-6.8.0-10-generic"

    def test_no_kernels_exits(self):
        with pytest.raises(SystemExit):
            eki.select_kernel_and_initrd(["initrd.img-6.12.0-14-generic"], None)

    def test_no_initrds_exits(self):
        with pytest.raises(SystemExit):
            eki.select_kernel_and_initrd(["vmlinuz-6.12.0-14-generic"], None)

    def test_missing_requested_kernel_exits(self):
        with pytest.raises(SystemExit):
            eki.select_kernel_and_initrd(BOOT_FILES, "99.99.0-1-generic")

    def test_missing_requested_initrd_exits(self):
        # Has kernel but no matching initrd
        files = [
            "vmlinuz-6.8.0-10-generic",
            "vmlinuz-7.0.0-1-generic",
            "initrd.img-6.8.0-10-generic",
        ]
        with pytest.raises(SystemExit):
            eki.select_kernel_and_initrd(files, "7.0.0-1-generic")

    def test_mismatched_initrd_exits(self):
        # Latest kernel exists but has no matching initrd - hard error, since a kernel and initrd must come from the same version.
        files = [
            "vmlinuz-6.12.0-14-generic",
            "vmlinuz-7.0.0-1-generic",
            "initrd.img-6.12.0-14-generic",
        ]
        with pytest.raises(SystemExit):
            eki.select_kernel_and_initrd(files, None)

    def test_empty_boot_files_exits(self):
        with pytest.raises(SystemExit):
            eki.select_kernel_and_initrd([], None)


# ---------------------------------------------------------------------------
# validate_output
# ---------------------------------------------------------------------------

class TestValidateOutput:
    def test_missing_file_exits(self, tmp_path):
        kernel = tmp_path / "vmlinuz-6.12.0-14-generic"
        initrd = tmp_path / "initrd.img-6.12.0-14-generic"
        with pytest.raises(SystemExit):
            eki.validate_output(kernel, initrd)

    def test_empty_file_exits(self, tmp_path):
        kernel = tmp_path / "vmlinuz-6.12.0-14-generic"
        initrd = tmp_path / "initrd.img-6.12.0-14-generic"
        kernel.write_bytes(b"")
        initrd.write_bytes(b"data")
        with pytest.raises(SystemExit):
            eki.validate_output(kernel, initrd)

    def test_valid_files_pass(self, tmp_path):
        kernel = tmp_path / "vmlinuz-6.12.0-14-generic"
        initrd = tmp_path / "initrd.img-6.12.0-14-generic"
        kernel.write_bytes(b"kernel-data")
        initrd.write_bytes(b"initrd-data")
        # Should not raise
        eki.validate_output(kernel, initrd)


# ---------------------------------------------------------------------------
# extract_files (path-traversal guard)
# ---------------------------------------------------------------------------

class TestExtractFilesSafety:
    def test_rejects_kernel_name_with_path_separator(self, tmp_path):
        # The path guard runs before the boot directory is used.
        with pytest.raises(SystemExit):
            eki.extract_files(
                tmp_path,
                "../../.ssh/authorized_keys",
                "initrd.img-6.12.0-14-generic",
                tmp_path,
            )

    def test_rejects_initrd_name_with_path_separator(self, tmp_path):
        with pytest.raises(SystemExit):
            eki.extract_files(
                tmp_path,
                "vmlinuz-6.12.0-14-generic",
                "../evil",
                tmp_path,
            )

    def test_rejects_absolute_path_name(self, tmp_path):
        # An absolute name collapses the join to itself, escaping output_dir.
        with pytest.raises(SystemExit):
            eki.extract_files(
                tmp_path,
                "/etc/passwd",
                "initrd.img-6.12.0-14-generic",
                tmp_path,
            )


class TestListBootFiles:
    def test_returns_sorted_directory_contents(self, tmp_path):
        (tmp_path / "vmlinuz-6.12.0-14-generic").touch()
        (tmp_path / "System.map-x").touch()
        (tmp_path / "config-x").touch()
        assert eki.list_boot_files(tmp_path) == [
            "System.map-x",
            "config-x",
            "vmlinuz-6.12.0-14-generic",
        ]

    def test_missing_directory_exits(self, tmp_path):
        with pytest.raises(SystemExit):
            eki.list_boot_files(tmp_path / "missing")


class TestExtractFiles:
    def test_copies_both_files(self, tmp_path):
        boot_dir = tmp_path / "boot"
        boot_dir.mkdir()
        (boot_dir / "vmlinuz-6.12.0-14-generic").write_bytes(b"kernel")
        (boot_dir / "initrd.img-6.12.0-14-generic").write_bytes(b"initrd")
        output_dir = tmp_path / "out"
        output_dir.mkdir()
        kernel_dst, initrd_dst = eki.extract_files(
            boot_dir,
            "vmlinuz-6.12.0-14-generic",
            "initrd.img-6.12.0-14-generic",
            output_dir,
        )
        assert kernel_dst.read_bytes() == b"kernel"
        assert initrd_dst.read_bytes() == b"initrd"

    def test_copy_failure_cleans_up_and_exits(self, tmp_path):
        boot_dir = tmp_path / "boot"
        boot_dir.mkdir()
        output_dir = tmp_path / "out"
        output_dir.mkdir()
        with pytest.raises(SystemExit):
            eki.extract_files(
                boot_dir,
                "vmlinuz-6.12.0-14-generic",
                "initrd.img-6.12.0-14-generic",
                output_dir,
            )
        assert list(output_dir.iterdir()) == []

    def test_cleanup_failure_preserves_extraction_error(self, tmp_path, monkeypatch):
        boot_dir = tmp_path / "boot"
        boot_dir.mkdir()
        output_dir = tmp_path / "out"
        output_dir.mkdir()

        def fail_unlink(self, *, missing_ok=False):
            raise PermissionError("cleanup denied")

        monkeypatch.setattr(Path, "unlink", fail_unlink)
        with pytest.raises(SystemExit, match="Extraction failed") as exc_info:
            eki.extract_files(
                boot_dir,
                "vmlinuz-6.12.0-14-generic",
                "initrd.img-6.12.0-14-generic",
                output_dir,
            )
        assert "No such file" in str(exc_info.value)

    def test_cleanup_failure_warns_on_stderr(self, tmp_path, monkeypatch, capsys):
        boot_dir = tmp_path / "boot"
        boot_dir.mkdir()
        (boot_dir / "vmlinuz-6.12.0-14-generic").write_bytes(b"kernel")
        (boot_dir / "initrd.img-6.12.0-14-generic").write_bytes(b"initrd")
        output_dir = tmp_path / "out"
        output_dir.mkdir()

        original_unlink = Path.unlink

        def fail_unlink(self, *, missing_ok=False):
            if self == output_dir / "vmlinuz-6.12.0-14-generic":
                raise PermissionError("cleanup denied")
            return original_unlink(self, missing_ok=missing_ok)

        monkeypatch.setattr(
            eki,
            "restore_output_ownership",
            lambda _: (_ for _ in ()).throw(OSError("simulated chown failure")),
        )
        monkeypatch.setattr(Path, "unlink", fail_unlink)
        with pytest.raises(SystemExit, match="Extraction failed"):
            eki.extract_files(
                boot_dir,
                "vmlinuz-6.12.0-14-generic",
                "initrd.img-6.12.0-14-generic",
                output_dir,
            )
        captured = capsys.readouterr()
        assert "Warning: Failed to remove partial file" in captured.err
        assert "cleanup denied" in captured.err
        assert (output_dir / "vmlinuz-6.12.0-14-generic").exists()
        assert not (output_dir / "initrd.img-6.12.0-14-generic").exists()

    def test_partial_copy_is_cleaned_up(self, tmp_path):
        boot_dir = tmp_path / "boot"
        boot_dir.mkdir()
        (boot_dir / "vmlinuz-6.12.0-14-generic").write_bytes(b"kernel")
        output_dir = tmp_path / "out"
        output_dir.mkdir()
        with pytest.raises(SystemExit):
            eki.extract_files(
                boot_dir,
                "vmlinuz-6.12.0-14-generic",
                "initrd.img-6.12.0-14-generic",
                output_dir,
            )
        assert list(output_dir.iterdir()) == []

    def test_ownership_failure_cleans_up(self, tmp_path, monkeypatch):
        boot_dir = tmp_path / "boot"
        boot_dir.mkdir()
        (boot_dir / "vmlinuz-6.12.0-14-generic").write_bytes(b"kernel")
        (boot_dir / "initrd.img-6.12.0-14-generic").write_bytes(b"initrd")
        output_dir = tmp_path / "out"
        output_dir.mkdir()
        monkeypatch.setattr(
            eki,
            "restore_output_ownership",
            lambda _: (_ for _ in ()).throw(OSError("simulated chown failure")),
        )

        with pytest.raises(SystemExit):
            eki.extract_files(
                boot_dir,
                "vmlinuz-6.12.0-14-generic",
                "initrd.img-6.12.0-14-generic",
                output_dir,
            )
        assert list(output_dir.iterdir()) == []


# ---------------------------------------------------------------------------
# NBD mount setup and cleanup
# ---------------------------------------------------------------------------

class TestEnsureNbdModuleLoaded:
    def test_returns_when_module_loaded(self, monkeypatch):
        monkeypatch.setattr(Path, "exists", lambda _: True)
        # Should not raise
        eki.ensure_nbd_module_loaded()

    def test_exits_when_module_not_loaded(self, monkeypatch):
        monkeypatch.setattr(Path, "exists", lambda _: False)
        with pytest.raises(SystemExit, match="nbd kernel module is not loaded"):
            eki.ensure_nbd_module_loaded()

    def test_error_message_includes_modprobe_guidance(self, monkeypatch):
        monkeypatch.setattr(Path, "exists", lambda _: False)
        with pytest.raises(SystemExit) as exc_info:
            eki.ensure_nbd_module_loaded()
        msg = str(exc_info.value)
        assert "sudo modprobe nbd" in msg
        assert "/etc/modules-load.d/nbd.conf" in msg

    def test_find_unused_nbd_device_calls_ensure_nbd_module_loaded(self, monkeypatch):
        """find_unused_nbd_device exits early when the nbd module is missing."""
        monkeypatch.setattr(Path, "exists", lambda _: False)
        with pytest.raises(SystemExit, match="nbd kernel module is not loaded"):
            eki.find_unused_nbd_device()


class TestFindUnusedNbdDevice:
    def test_selects_first_disconnected_base_block_device(self, monkeypatch, tmp_path):
        # nbd0 has a pid file (busy), nbd2 has no pid file and size==0 (free)
        monkeypatch.setattr(
            eki.glob, "glob",
            lambda _: ["/dev/nbd2", "/dev/nbd0p1", "/dev/nbd1", "/dev/nbd0"],
        )
        monkeypatch.setattr(
            Path, "is_block_device",
            lambda path: path.name in {"nbd0", "nbd2"},
        )

        pid_exists = {"/sys/module/nbd": True, "/sys/block/nbd0/pid": True, "/sys/block/nbd2/pid": False}
        size_values = {"/sys/block/nbd0/size": "0\n", "/sys/block/nbd2/size": "0\n"}

        _original_exists = Path.exists

        def patched_exists(path):
            key = str(path).replace("\\", "/")
            if key in pid_exists:
                return pid_exists[key]
            return _original_exists(path)

        def patched_read_text(path):
            key = str(path).replace("\\", "/")
            if key in size_values:
                return size_values[key]
            raise OSError("not mocked")

        monkeypatch.setattr(Path, "exists", patched_exists)
        monkeypatch.setattr(Path, "read_text", patched_read_text)

        assert eki.find_unused_nbd_device() == Path("/dev/nbd2")

    def test_skips_device_with_pid_file_present(self, monkeypatch):
        monkeypatch.setattr(eki.glob, "glob", lambda _: ["/dev/nbd0"])
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)
        monkeypatch.setattr(Path, "exists", lambda _: True)

        with pytest.raises(SystemExit, match="No unused /dev/nbdN"):
            eki.find_unused_nbd_device()

    def test_skips_device_with_nonzero_size(self, monkeypatch, tmp_path):
        monkeypatch.setattr(eki.glob, "glob", lambda _: ["/dev/nbd0"])
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)
        monkeypatch.setattr(Path, "exists", lambda p: str(p) == "/sys/module/nbd")
        monkeypatch.setattr(Path, "read_text", lambda _: "2097152\n")

        with pytest.raises(SystemExit, match="No unused /dev/nbdN"):
            eki.find_unused_nbd_device()

    def test_skips_device_with_unreadable_size(self, monkeypatch):
        monkeypatch.setattr(eki.glob, "glob", lambda _: ["/dev/nbd0"])
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)
        monkeypatch.setattr(Path, "exists", lambda p: str(p) == "/sys/module/nbd")

        def unreadable_size(_: Path) -> str:
            raise OSError("unavailable")

        monkeypatch.setattr(Path, "read_text", unreadable_size)

        with pytest.raises(SystemExit, match="No unused /dev/nbdN"):
            eki.find_unused_nbd_device()

    def test_returns_device_when_no_pid_and_size_zero(self, monkeypatch):
        monkeypatch.setattr(eki.glob, "glob", lambda _: ["/dev/nbd0"])
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)
        monkeypatch.setattr(Path, "exists", lambda p: str(p) == "/sys/module/nbd")
        monkeypatch.setattr(Path, "read_text", lambda _: "0\n")

        assert eki.find_unused_nbd_device() == Path("/dev/nbd0")


class TestPartitionPaths:
    def test_calls_udevadm_settle_when_available(self, monkeypatch, tmp_path):
        device = tmp_path / "nbd0"
        partition = tmp_path / "nbd0p1"
        partition.touch()
        commands_run = []

        monkeypatch.setattr(eki.shutil, "which", lambda cmd: "/usr/bin/udevadm" if cmd == "udevadm" else None)
        monkeypatch.setattr(
            eki.subprocess, "run",
            lambda cmd, **kw: commands_run.append(cmd) or subprocess.CompletedProcess(cmd, 0),
        )
        monkeypatch.setattr(eki.glob, "glob", lambda _: [str(partition)])
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)

        result = eki.partition_paths(device)
        assert ["udevadm", "settle"] in commands_run
        assert result == [partition]

    def test_polls_after_udevadm_when_partitions_appear_late(self, monkeypatch, tmp_path):
        device = tmp_path / "nbd0"
        partition = tmp_path / "nbd0p1"
        partition.touch()
        commands_run = []
        call_count = {"n": 0}

        monkeypatch.setattr(eki.shutil, "which", lambda cmd: "/usr/bin/udevadm" if cmd == "udevadm" else None)
        monkeypatch.setattr(
            eki.subprocess, "run",
            lambda cmd, **kw: commands_run.append(cmd) or subprocess.CompletedProcess(cmd, 0),
        )
        monkeypatch.setattr(eki.time, "sleep", lambda _: None)

        def delayed_glob(_):
            call_count["n"] += 1
            return [str(partition)] if call_count["n"] >= 3 else []

        monkeypatch.setattr(eki.glob, "glob", delayed_glob)
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)

        result = eki.partition_paths(device)
        assert ["udevadm", "settle"] in commands_run
        assert result == [partition]
        assert call_count["n"] == 3

    def test_skips_udevadm_when_unavailable(self, monkeypatch, tmp_path):
        device = tmp_path / "nbd0"
        partition = tmp_path / "nbd0p1"
        partition.touch()
        commands_run = []

        monkeypatch.setattr(eki.shutil, "which", lambda _: None)
        monkeypatch.setattr(
            eki.subprocess, "run",
            lambda cmd, **kw: commands_run.append(cmd) or subprocess.CompletedProcess(cmd, 0),
        )
        monkeypatch.setattr(eki.glob, "glob", lambda _: [str(partition)])
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)

        result = eki.partition_paths(device)
        assert ["udevadm", "settle"] not in commands_run
        assert result == [partition]

    def test_polls_until_partitions_appear(self, monkeypatch, tmp_path):
        device = tmp_path / "nbd0"
        partition = tmp_path / "nbd0p1"
        partition.touch()
        call_count = {"n": 0}

        monkeypatch.setattr(eki.shutil, "which", lambda _: None)
        monkeypatch.setattr(eki.time, "sleep", lambda _: None)

        def delayed_glob(_):
            call_count["n"] += 1
            return [str(partition)] if call_count["n"] >= 3 else []

        monkeypatch.setattr(eki.glob, "glob", delayed_glob)
        monkeypatch.setattr(Path, "is_block_device", lambda _: True)

        result = eki.partition_paths(device)
        assert result == [partition]
        assert call_count["n"] == 3

    def test_exits_when_no_partitions_appear(self, monkeypatch, tmp_path):
        device = tmp_path / "nbd0"

        monkeypatch.setattr(eki.shutil, "which", lambda _: None)
        monkeypatch.setattr(eki.time, "sleep", lambda _: None)
        monkeypatch.setattr(eki.glob, "glob", lambda _: [])
        monkeypatch.setattr(eki, "PARTITION_POLL_ATTEMPTS", 2)

        with pytest.raises(SystemExit, match="No partitions appeared"):
            eki.partition_paths(device)


class TestNbdPartitionCapacity:
    def test_allows_an_image_within_the_module_limit(self, monkeypatch, tmp_path):
        monkeypatch.setattr(eki, "nbd_partition_capacity", lambda: 15)
        monkeypatch.setattr(eki, "image_partition_numbers", lambda _: [1, 13])
        eki.validate_nbd_partition_capacity(tmp_path / "nbd0")

    def test_reports_a_reload_command_when_limit_is_too_low(self, monkeypatch, tmp_path):
        monkeypatch.setattr(eki, "nbd_partition_capacity", lambda: 8)
        monkeypatch.setattr(eki, "image_partition_numbers", lambda _: [1, 13])

        with pytest.raises(SystemExit) as error:
            eki.validate_nbd_partition_capacity(tmp_path / "nbd0")

        assert "max_part=16" in str(error.value)


class TestMountedBootDirectory:
    def test_connects_read_only_and_cleans_up(self, tmp_path, monkeypatch):
        image = tmp_path / "image.qcow2"
        image.touch()
        device = tmp_path / "nbd0"
        partition = tmp_path / "nbd0p13"
        partition.touch()
        commands = []

        def fake_run(command):
            commands.append(command)

        # Mounting, unmounting and disconnecting all go through subprocess.run so the stub reports a successful return code.
        def fake_subprocess_run(command, **kwargs):
            commands.append(command)
            if command[0] == "mount":
                target = Path(command[-1])
                (target / "vmlinuz-6.12.0-14-generic").touch()
            return subprocess.CompletedProcess(command, 0)

        monkeypatch.setattr(eki, "find_unused_nbd_device", lambda: device)
        monkeypatch.setattr(eki, "validate_nbd_partition_capacity", lambda _: None)
        monkeypatch.setattr(eki, "partition_paths", lambda _: [partition])
        monkeypatch.setattr(eki, "run", fake_run)
        monkeypatch.setattr(eki.subprocess, "run", fake_subprocess_run)

        with eki.mounted_boot_directory(image) as boot_dir:
            assert boot_dir.name.startswith("tdx-boot-")

        assert commands[0] == ["qemu-nbd", "--read-only", "--connect", str(device), str(image)]
        assert ["umount", str(boot_dir)] in commands
        assert ["qemu-nbd", "--disconnect", str(device)] in commands

    def test_skips_unmountable_partitions(self, tmp_path, monkeypatch):
        image = tmp_path / "image.qcow2"
        image.touch()
        device = tmp_path / "nbd0"
        bad_partition = tmp_path / "nbd0p1"
        good_partition = tmp_path / "nbd0p2"
        bad_partition.touch()
        good_partition.touch()

        def fake_run(command):
            pass

        def fake_subprocess_run(command, **kwargs):
            if command[0] == "mount":
                target = Path(command[-1])
                if command[2] == str(bad_partition):
                    return subprocess.CompletedProcess(command, 32)
                (target / "vmlinuz-6.12.0-14-generic").touch()
                return subprocess.CompletedProcess(command, 0)
            return subprocess.CompletedProcess(command, 0)

        monkeypatch.setattr(eki, "find_unused_nbd_device", lambda: device)
        monkeypatch.setattr(eki, "validate_nbd_partition_capacity", lambda _: None)
        monkeypatch.setattr(eki, "partition_paths", lambda _: [bad_partition, good_partition])
        monkeypatch.setattr(eki, "run", fake_run)
        monkeypatch.setattr(eki.subprocess, "run", fake_subprocess_run)

        with eki.mounted_boot_directory(image) as boot_dir:
            assert any(boot_dir.glob("vmlinuz-*"))

    def test_prints_confirmation_on_successful_disconnect(self, tmp_path, monkeypatch, capsys):
        image = tmp_path / "image.qcow2"
        image.touch()
        device = tmp_path / "nbd0"

        def fake_run(command):
            pass

        def fake_subprocess_run(command, **kwargs):
            if command[0] == "mount":
                target = Path(command[-1])
                (target / "vmlinuz-6.12.0-14-generic").touch()
            return subprocess.CompletedProcess(command, 0)

        monkeypatch.setattr(eki, "find_unused_nbd_device", lambda: device)
        monkeypatch.setattr(eki, "validate_nbd_partition_capacity", lambda _: None)
        monkeypatch.setattr(eki, "partition_paths", lambda _: [tmp_path / "nbd0p1"])
        monkeypatch.setattr(eki, "run", fake_run)
        monkeypatch.setattr(eki.subprocess, "run", fake_subprocess_run)

        with eki.mounted_boot_directory(image) as boot_dir:
            pass

        captured = capsys.readouterr()
        assert f"Disconnected the image from '{device}'" in captured.out

    def test_prints_warning_on_failed_disconnect(self, tmp_path, monkeypatch, capsys):
        image = tmp_path / "image.qcow2"
        image.touch()
        device = tmp_path / "nbd0"

        def fake_run(command):
            pass

        def fake_subprocess_run(command, **kwargs):
            if command[0] == "mount":
                target = Path(command[-1])
                (target / "vmlinuz-6.12.0-14-generic").touch()
                return subprocess.CompletedProcess(command, 0)
            if command[0] == "qemu-nbd" and "--disconnect" in command:
                return subprocess.CompletedProcess(command, 1)
            return subprocess.CompletedProcess(command, 0)

        monkeypatch.setattr(eki, "find_unused_nbd_device", lambda: device)
        monkeypatch.setattr(eki, "validate_nbd_partition_capacity", lambda _: None)
        monkeypatch.setattr(eki, "partition_paths", lambda _: [tmp_path / "nbd0p1"])
        monkeypatch.setattr(eki, "run", fake_run)
        monkeypatch.setattr(eki.subprocess, "run", fake_subprocess_run)

        with eki.mounted_boot_directory(image) as boot_dir:
            pass

        captured = capsys.readouterr()
        assert f"Warning: Failed to disconnect the image from '{device}'" in captured.err
        assert f"qemu-nbd --disconnect {device}" in captured.err


# ---------------------------------------------------------------------------
# main (CLI argument parsing)
# ---------------------------------------------------------------------------

class TestMainCli:
    def test_missing_python_apt_exits_with_installation_command(self):
        result = subprocess.run(
            [
                sys.executable,
                "-S",
                str(Path(__file__).resolve().parent.parent / "extract_kernel_initrd.py"),
                "--help",
            ],
            capture_output=True,
            text=True,
        )
        assert result.returncode != 0
        assert "python3-apt is not installed" in result.stderr
        assert "sudo apt install python3-apt" in result.stderr

    def test_help_exits_zero(self):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).resolve().parent.parent / "extract_kernel_initrd.py"), "--help"],
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0
        assert "Extract kernel and initrd" in result.stdout

    def test_missing_image_exits_nonzero(self, tmp_path):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).resolve().parent.parent / "extract_kernel_initrd.py"), str(tmp_path / "nonexistent.qcow2")],
            capture_output=True,
            text=True,
        )
        assert result.returncode != 0
        assert "not found" in result.stderr

    def test_root_hint_uses_invoked_script_name(self, tmp_path, monkeypatch):
        dummy_image = tmp_path / "dummy.qcow2"
        dummy_image.write_bytes(b"fake")

        monkeypatch.setattr(eki.os, "geteuid", lambda: 1000)
        monkeypatch.setattr(sys, "argv", ["renamed-extractor", str(dummy_image)])

        with pytest.raises(SystemExit, match="sudo python3 renamed-extractor"):
            eki.main()

    def test_missing_qemu_nbd_exits_nonzero(self, tmp_path, monkeypatch):
        dummy_image = tmp_path / "dummy.qcow2"
        dummy_image.write_bytes(b"fake")

        monkeypatch.setattr(eki.os, "geteuid", lambda: 0)
        monkeypatch.setattr(eki.shutil, "which", lambda _: None)
        monkeypatch.setattr(sys, "argv", ["extract_kernel_initrd.py", str(dummy_image)])

        with pytest.raises(SystemExit):
            eki.main()
