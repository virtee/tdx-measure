# tdx-measure

## Scope
Command-line tool and Rust library to calculate expected measurement of an Intel TDX guest VM for confidential computing.

The `tdx-measure` tool takes a set of image binaries and platform config files as an input and outputs the corresponding TDX measurements. This makes it possible to exhaustively publish all images and config files that uniquely identify a TDX workload on a machine, making TD environments transparent and auditable.

Currently, the tool is able to pre-calculate measurements for the boot chains from the Canonical's Intel TDX [repo](https://github.com/canonical/tdx).
In particular, the measurement calculation was validated for TDs using Ubuntu 25.04 as guest OS running on Ubuntu 25.04 as host OS.
The building and execution of `tdx-measure` was validated on Ubuntu 25.04.

### Acknowledgment
This project is a fork of dstack-mr from the [Dstack-TEE/dstack](https://github.com/Dstack-TEE/dstack) repository.

## Usage

```tdx-measure [OPTIONS] <METADATA>```

### Arguments:
`<METADATA>` Path to metadata json file (see format lower down)

### Options
```
      --direct-boot <DIRECT_BOOT>           Enable direct boot (overrides JSON configuration) [possible values: true, false]
      --json                                Output JSON
      --json-file <JSON_FILE>               Output JSON to file
      --platform-only                       Compute MRTD and RTMR0 only
      --runtime-only                        Compute RTMR1 and RTMR2 only
      --transcript <TRANSCRIPT>             Generate a human-readable transcript of all metadata files and write to the specified file
      --create-acpi-tables <DISTRIBUTION>   Generate ACPI tables for direct boot mode. Only valid with direct boot. [possible values: ubuntu:25.04, ubuntu:26.04]
  -h, --help                                Print help
  -V, --version                             Print version
```

WARNING: when running with `--runtime-only`, the tool will assume a VM memory size higher that 2.75GB.

### Direct Boot

#### Metadata

Create `metadata.json` file with the below metadata:

```
{
  "boot_config": {
    "cpus": 16,
    "memory": "2G",
    "bios": "[path to OVMF.fd]",
    "acpi_tables": "[path to acpi_tables.bin]",
    "rsdp": "[path to rsdp.bin]",
    "table_loader": "[path to table_loader.bin]"
  },
  "direct": {
    "kernel": "[path to vmlinuz]",
    "initrd": "[path to initrd]",
    "cmdline": "root=/dev/sda1 console=ttyS0"
  }
}
```

#### Metadata Field Descriptions

- `boot_config`: Platform configuration used to compute MRTD and RTMR[0]
  - `cpus`: Number of virtual CPUs allocated to the TD.
  - `memory`: The amount of memory allocated to the TD (e.g., "2G" for 2 gigabytes).
  - `bios`: Path to file (e.g., `OVMF.fd`) containing a virtual BIOS, which is used to boot the TD image.
    The file can be obtained by setting up the host OS following [these instructions](https://github.com/canonical/tdx/tree/main?tab=readme-ov-file#4-setup-host-os) and retrieving it from `/usr/share/ovmf/OVMF.fd`.
  - `acpi_tables`: Path to a file containing ACPI tables, which describe the hardware configuration and device tree that the TD uses to discover and configure hardware.
    By using the `--create-acpi-tables` flag of the `tdx-measure` tool, the ACPI tables are generated automatically and stored to a file at the path provided by `acpi_tables`.
    Alternatively, a file containing the ACPI tables can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `rsdp`: Path to a file containing a Root System Description Pointer (RSDP), which is an ACPI data structure that provides the address of the RSDT/XSDT table.
    By using the `--create-acpi-tables` flag of the `tdx-measure` tool, the ACPI tables are generated automatically and the RSDP is derived from these tables automatically.
    In this case, this flag is not needed.
    Alternatively, a file containing the RSDP can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
    In this case, the extracted file can be provided with this flag.
  - `table_loader`: Path to a file containing a ACPI table loader, which contains QEMU-specific commands for loading and patching ACPI tables.
    By using the `--create-acpi-tables` flag of the `tdx-measure` tool, the ACPI tables are generated automatically and the ACPI table loader is derived from these tables automatically.
    In this case, this flag is not needed.
    Alternatively, a file containing the RSDP can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
    In this case, the extracted file can be provided with this flag.

- `direct`: Direct boot specific configuration used to compute RTMR[1] and RTMR[2]
  - `kernel`: Path to file (e.g., `vmlinuz`) of kernel image, which will be directly loaded and executed by OVMF.
    The file can be obtain by following [these instructions](https://github.com/canonical/tdx/tree/main/guest-tools/direct-boot#prerequisites).
  - `initrd`: Path to file (e.g., `initrd.img`) of initial RAM disk, which is a temporary root filesystem loaded into memory during boot, containing drivers and tools needed to mount the actual root filesystem.
    The file can be obtain by following the [these instructions](https://github.com/canonical/tdx/tree/main/guest-tools/direct-boot#prerequisites).
  - `cmdline`: Kernel command line parameters.
    These parameters specify the kernel command line arguments that are passed to QEMU using the `-append` option.

Note: For direct boot, `boot_order` and `path_boot_xxxx` do not need to be specified in the metadata file, as there is only one standardized BootOrder variable and a corresponding Boot#### UEFI variable.
These are calculated by the tool automatically.

### Indirect Boot

#### Metadata

Create `metadata.json` file with the below metadata:

```
{
  "boot_config": {
    "cpus": 32,
    "memory": "10G",
    "bios": "[path to OVMF.fd]",
    "acpi_tables": "[path to acpi_tables.bin]",
    "rsdp": "[path to rsdp.bin]",
    "table_loader": "[path to table_loader.bin]",
    "boot_order": "[path to BootOrder.bin]",
    "path_boot_xxxx": "[path to directory containing Bootxxxx.bin files]"
  },
  "indirect": {
    "qcow2": "[path to tdx-guest-ubuntu-25.04-generic.qcow2]",
    "cmdline": "console=ttyS0 root=/dev/vda1",
    "mok_list": "[path to MokList.bin]",
    "mok_list_trusted": "[path to MokListTrusted.bin]",
    "mok_list_x": "[path to MokListX.bin]",
    "sbat_level": "[path to SbatLevel.bin]"
  }
}
```

#### Metadata Field Descriptions

- `boot_config`: Platform configuration used to compute MRTD and RTMR[0]
  - `cpus`: Number of virtual CPUs allocated to the TD.
  - `memory`: The amount of memory allocated to the TD (e.g., "2G" for 2 gigabytes).
  - `bios`: Path to file (e.g., `OVMF.fd`) containing a virtual BIOS, which is used to boot the TD image.
    The file can be obtained by setting up the host OS following [these instructions](https://github.com/canonical/tdx/tree/main?tab=readme-ov-file#4-setup-host-os) and retrieving it from `/usr/share/ovmf/OVMF.fd`.
  - `acpi_tables`: Path to file containing ACPI tables, which describe the hardware configuration and device tree that the TD uses to discover and configure hardware.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `rsdp`: Path to file containing a Root System Description Pointer (RSDP), which is an ACPI data structure that provides the address of the RSDT/XSDT table.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `table_loader`: Path to file containing a ACPI table loader, which contains QEMU-specific commands for loading and patching ACPI tables.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `boot_order`: Path to file containing a UEFI BootOrder variable, which specifies the order in which the firmware attempts to boot from different boot options.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `path_boot_xxxx`: Path to directory containing files (e.g., `Boot0000.bin`, `Boot0001.bin`, `Boot0002.bin`) for each Boot#### UEFI variables.
    Each variable defines a specific boot option with its device path and description.
    These files can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.

- `indirect`: Indirect boot specific configuration used to compute RTMR[1] and RTMR[2]
  - `qcow2`: Path to guest OS disk image in QCOW2 format.
    The image contains the guest filesystem with the bootloader chain (e.g., SHIM and Grub) and a kernel.
    The image can be created by following [these instructions](https://github.com/canonical/tdx/tree/main?tab=readme-ov-file#5-create-td-image).
  - `cmdline`: Kernel command line parameters.
    These parameters can be obtained by executing `cat /proc/cmdline` inside a TD, which is configured identically to the target configuration.
  - `mok_list`: Path to file containing a Machine Owner Key (MOK) list for Secure Boot.
    Contains user-enrolled keys that supplement the vendor-provided Secure Boot keys.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `mok_list_trusted`: Path to file containing a MOK trusted list.
    Contains additional trusted keys for Secure Boot verification.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `mok_list_x`: Path to file containing a MOK blacklist (forbidden keys).
    Contains keys that have been explicitly revoked and should not be trusted.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.
  - `sbat_level`: Path to file containing a SBAT (Secure Boot Advanced Targeting) revocation list.
    Used to revoke specific bootloader versions without revoking their signing keys.
    The file can be extracted by running the [`extract_config_files.py`](extract_config_files.py) script inside a TD, which is configured identically to the target configuration.

### Transcript

The transcript flag makes it possible to generate a human-readable transcript from the different binary configuration files. The command line tool `iasl` needs to be installed in order to disassembled the ACPI tables and include its representation in the transcript.

## Prerequisite

### Install Rust

[If not already done] Install Rust dependency, install Rust, activate it in the current shell, and test the installation:
```
sudo apt update
sudo apt install build-essential
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo --version
```
See [Rust's installation documentation](https://www.rust-lang.org/tools/install) for more detailed information.

### Install jq

> [!NOTE]
> This prerequisite is only required when generating ACPI tables in Direct Mode with `--create-acpi-tables`.

[If not already done] Install jq with the following command:
```
sudo apt-get install jq
```

### Install Docker

> [!NOTE]
> This prerequisite is only required when generating ACPI tables in Direct Mode with `--create-acpi-tables`.

> [!NOTE]
> See [Docker's installation documentation](https://docs.docker.com/engine/install/ubuntu/) and [Docker's post-installation steps](https://docs.docker.com/engine/install/linux-postinstall/) for more detailed information.

[If not already done] Install Docker with the following steps:

1. Add Docker's official GPG key:
    ```
    sudo apt update
    sudo apt install ca-certificates curl
    sudo install -m 0755 -d /etc/apt/keyrings
    sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
    sudo chmod a+r /etc/apt/keyrings/docker.asc
    ```

2. Add Docker's package repository:
    ```
    sudo tee /etc/apt/sources.list.d/docker.sources <<EOF
    Types: deb
    URIs: https://download.docker.com/linux/ubuntu
    Suites: $(. /etc/os-release && echo "${UBUNTU_CODENAME:-$VERSION_CODENAME}")
    Components: stable
    Signed-By: /etc/apt/keyrings/docker.asc
    EOF
    ```

3. Install the Docker packages:
    ```
    sudo apt update
    sudo apt install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
    ```

4. Manage Docker as a non-root user:
    ```
    sudo groupadd docker
    sudo usermod -aG docker $USER
    ```

    - Log out and log back in so that your group membership is re-evaluated.
    - Alternatively, execute the following command to activate the changes to groups:
        ```
        newgrp docker
        ```

## Build the project

```
cargo build --release
```

## Install the CLI tool

```
cargo install --path cli
```



## Info

### Boot Methods
Canonical repo offer two boot options:

1) Direct Boot:
With this method, `OVMF` (the TDVF or virtual firmware) directly boots the kernel image. In this mode, the `kernel`, `initrd` and the kernel `cmdline` are directly supplied to `qemu`.

2) Indirect Boot:
With this method, `tdvirsh` is used to run TDs, the boot chain is more complex and involves `OVMF`, a `SHIM`, `Grub`, and finally the `kernel`+`initrd` image.

### What goes in the measurements

TDX attestation reports expose 4 measurement registers (MR).

The first one, `MRTD`, represent the measurements for the TD virtual firmware binary (TDVF, specifically OVMF.fd in our case).

Three other runtime measurement registers (`RTMR`) correspond to different boot stages and vary depending on the boot chain.

`RTMR[0]` contains firmware configuration and platform specific measurements. This includes hashes of:
- The TD HOB which mostly contains a description of the memory accessible to the TD.
- TDX configuration values.
- Various Secure Boot variables.
- ACPI tables that describe the device tree.
- Boot variables (BootOrder and others).
- [for indirect boot only] [SbatLevel](https://github.com/rhboot/shim/blob/main/SbatLevel_Variable.txt) variable.

`RTMR[1]` contains measurements of the `kernel` for direct boot. For indirect boot, it contains measurement for the bootchain a.k.a. `gpt` (GUID Partition Table), `shim`, and `grub`.

`RTMR[2]` contains measurements of the kernel `cmdline` and `initrd` for direct boot. For indirect boot, it also contains the measurements of machine owner key [(MOK) variables](https://github.com/rhboot/shim/blob/main/MokVars.txt).
