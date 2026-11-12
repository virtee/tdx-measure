# tdx-measure

## Scope
Command-line tool and Rust library to calculate expected measurement of an Intel TDX guest VM for confidential computing.

The `tdx-measure` tool takes a set of image binaries and platform config files as an input and outputs the corresponding TDX measurements. This makes it possible to exhaustively publish all images and config files that uniquely identify a TDX workload on a machine, making TD environments transparent and auditable.

Currently, the tool is able to pre-calculate measurements for the boot chains from the Canonical's Intel TDX [repo](https://github.com/canonical/tdx) for Ubuntu 25.04, and from [Ubuntu's Intel TDX setup guide](https://ubuntu.com/server/docs/how-to/virtualisation/intel-tdx/) for Ubuntu 26.04.
In particular, the measurement calculation was validated for TDs using Ubuntu 25.04 or Ubuntu 26.04 as guest OS running on the corresponding Ubuntu version as host OS.
The building and execution of `tdx-measure` was validated on Ubuntu 25.04 and Ubuntu 26.04.

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
      --create-acpi-tables <DISTRIBUTION> [<QEMU_VERSION>]
                                            Generate ACPI tables for direct boot mode. Only valid with direct boot.
                                            <DISTRIBUTION> in {ubuntu:25.04, ubuntu:26.04}.
                                            <QEMU_VERSION> overrides the pinned QEMU source-package version for the given distribution
                                            (pass `""` to fetch the current main-archive tip).
  -h, --help                                Print help
  -V, --version                             Print version
```

WARNING: when running with `--runtime-only`, the tool will assume a VM memory size higher that 2.75GB.

### Direct Boot

> **Supported distributions:** Ubuntu 25.04, Ubuntu 26.04

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
    - **Ubuntu 25.04**: To use the OVMF binary shipped with Ubuntu 25.04, follow [these instructions](https://github.com/canonical/tdx/tree/main?tab=readme-ov-file#4-setup-host-os) and retrieve the file from `/usr/share/ovmf/OVMF.fd`.
    - **Ubuntu 26.04**: As the measurement tool currently only supports non-secure-boot OVMF binaries and Ubuntu 26.04 only brings a secure-boot OVMF for Intel TDX, we cannot use it directly.
      The following steps can be used to build a non-secure-boot OVMF binary for Intel TDX, which is as close as possible to the one provided by Ubuntu 26.04:

      <details>
      <summary><b><a id="building-ovmf-from-source-ubuntu-2604"></a>Building OVMF from source (Ubuntu 26.04)</b></summary>

      1. Install build dependencies (skip if already installed):
          ```bash
          sudo apt install build-essential nasm acpica-tools uuid-dev
          ```

      2. Download and extract the edk2 source used by Ubuntu 26.04.
          Note that we use the version `2025.11-3ubuntu7.2`, which was the current stable release for Ubuntu 26.04 (Resolute Raccoon) at the time of writing.
          Please check [the edk2 package in Ubuntu page](https://launchpad.net/ubuntu/+source/edk2/) for the newest stable release.
          If a newer version is used, we recommend adjusting the following commands accordingly.
          ```bash
          wget https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/edk2/2025.11-3ubuntu7.2/edk2_2025.11.orig.tar.xz
          wget https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/edk2/2025.11-3ubuntu7.2/edk2_2025.11-3ubuntu7.2.debian.tar.xz
          wget https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/edk2/2025.11-3ubuntu7.2/edk2_2025.11-3ubuntu7.2.dsc
          dpkg-source -x edk2_2025.11-3ubuntu7.2.dsc
          ```

      3. Build OVMF by mirroring the `build-ovmf-inteltdx-ms` target in `debian/rules`, with the single change of `-DSECURE_BOOT_ENABLE=FALSE` instead of `TRUE`:
          ```bash
          cd edk2-2025.11
          make -C BaseTools
          source edksetup.sh
          DEB_VERSION=$(dpkg-parsechangelog -l debian/changelog -S Version)
          SOURCE_DATE_EPOCH=$(dpkg-parsechangelog -l debian/changelog -S Timestamp)
          RELEASE_DATE=$(date -d@"$SOURCE_DATE_EPOCH" "+%m/%d/%Y")
          VENDOR="$(lsb_release -is) distribution of EDK II"
          build -p OvmfPkg/IntelTdx/IntelTdxX64.dsc -a X64 -t GCC5 -b RELEASE \
            -DCC_MEASUREMENT_ENABLE=TRUE -DNETWORK_HTTP_BOOT_ENABLE=TRUE \
            -DNETWORK_IP6_ENABLE=TRUE -DNETWORK_TLS_ENABLE \
            -DTPM2_ENABLE=TRUE -DFD_SIZE_4MB \
            -DBUILD_SHELL=FALSE -DSECURE_BOOT_ENABLE=FALSE -DSMM_REQUIRE=FALSE \
            --pcd PcdFirmwareVendor=L"$VENDOR\0" \
            --pcd PcdFirmwareVersionString=L"$DEB_VERSION\0" \
            --pcd PcdFirmwareReleaseDateString=L"$RELEASE_DATE\0"
          ```

          The resulting `OVMF.fd` is at `Build/IntelTdx/RELEASE_GCC5/FV/OVMF.fd`.

      4. Verify the existence of the OVMF binary:
          ```bash
          ls Build/IntelTdx/RELEASE_GCC5/FV/OVMF.fd
          ```

      </details>

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
  - `qemu` *(optional)*: Generic QEMU shape descriptor consumed by `--create-acpi-tables`.
    When present, the QEMU command is built from this block plus a minimal set of measurement-related core flags (`-smp ... -m ... -bios ... -nodefaults -vga none -nographic -no-reboot`).
    All fields except `machine` are passed verbatim; `machine` is normalized so `smm=off` and `pic=off` are present even when omitted.
    When absent, the tool falls back to the Canonical direct-boot args (see Canonical's [`boot_direct.sh`](https://github.com/canonical/tdx/blob/a599cf54510ec81667cedfb183e4da9f96be536d/guest-tools/direct-boot/boot_direct.sh#L54-L68)). If your TD configuration does not match Canonical's, you need to provide the QEMU shape matching your TD configuration, otherwise the tool's `RTMR[0]` prediction will not match the value measured by a real TD.
    Fields:
    - `machine` *(required when block is present)*: value passed to `-machine`, with `smm=off` and `pic=off` appended when omitted. When duplicates exist, QEMU last-wins semantics apply: only the final value of each flag is checked, so `smm=on,smm=off` is accepted (effective value `off`) while `smm=off,smm=on` is rejected (effective value `on`). Example: `"q35,kernel_irqchip=split,memory-backend=mem0"`.
    - `cpu` *(optional, default `"host"`)*: value passed verbatim to `-cpu`. Use a named model with an explicit `phys-bits=N` (e.g. `"Skylake-Server,phys-bits=46"`) to make the run independent of the host CPU.
    - `accel` *(optional, default `"kvm"`)*: value passed verbatim to `-accel`. Use `"tcg"` to skip KVM (CI runners without `/dev/kvm`, ARM hosts via x86 emulation, …).
    - `globals`: list of strings, each emitted as `-global <value>`.
    - `objects`: list of strings, each emitted as `-object <value>`.
    - `netdevs`: list of strings, each emitted as `-netdev <value>`.
    - `devices`: list of strings, each emitted as `-device <value>`. **Order is part of the API contract**: QEMU's PCI auto-slot assignment iterates `-device` arguments in command-line order and picks the lowest free slot, so `devices` must be authored in the same order as the reference launch you are mirroring. A `slot=N` argument on a `pcie-root-port` is only a hint and shifts if the slot is already taken by an earlier auto-assigned device. Order within `globals` / `objects` / `netdevs` / `fw_cfg` does not influence the ACPI tables.
    - `fw_cfg`: list of strings, each emitted as `-fw_cfg <value>`.

- `direct`: Direct boot specific configuration used to compute RTMR[1] and RTMR[2]
  - `kernel`: Path to file (e.g., `vmlinuz`) of kernel image, which will be directly loaded and executed by OVMF.
    - **Ubuntu 25.04**: The file can be obtained by following [these instructions](https://github.com/canonical/tdx/tree/main/guest-tools/direct-boot#prerequisites).
    - **Ubuntu 26.04**: Download the TD guest cloud image and extract the kernel and initrd from it:
        ```bash
        wget https://cloud-images.ubuntu.com/resolute/current/resolute-server-cloudimg-amd64.img
        sudo apt install python3-apt qemu-utils
        sudo python3 extract_kernel_initrd.py resolute-server-cloudimg-amd64.img
        ```
        > **Note:** The extractor connects the image to an NBD device and mounts its partitions read-only, so it requires root privileges but does not use or need access to the host kernel image.
        > It automatically disconnects the NBD device and preserves ownership of extracted files for the user who invoked `sudo`.
        > If the loaded NBD module cannot expose enough image partitions, the extractor reports the required `modprobe` commands.
  - `initrd`: Path to file (e.g., `initrd.img`) of initial RAM disk, which is a temporary root filesystem loaded into memory during boot, containing drivers and tools needed to mount the actual root filesystem.
    - **Ubuntu 25.04**: The file can be obtained by following [these instructions](https://github.com/canonical/tdx/tree/main/guest-tools/direct-boot#prerequisites).
    - **Ubuntu 26.04**: Use the same guest image and [`extract_kernel_initrd.py`](extract_kernel_initrd.py) script as described for `kernel` above.
  - `cmdline`: Kernel command line parameters.
    These parameters specify the kernel command line arguments that are passed to QEMU using the `-append` option.

Note: For direct boot, `boot_order` and `path_boot_xxxx` do not need to be specified in the metadata file, as there is only one standardized BootOrder variable and a corresponding Boot#### UEFI variable.
These are calculated by the tool automatically.

#### Generating ACPI tables

When `--create-acpi-tables` is passed, the tool builds a small Docker image that
patches QEMU to dump `etc/acpi/tables` and exits before TD entry. This requires
a working `docker` (with buildx) and, when `accel: "kvm"` is in effect, KVM on
the host. Each supported distribution pins a known-good QEMU source version
(`1:9.2.1+ds-1ubuntu4+tdx2.0~ppa2` for `ubuntu:25.04`, `1:10.2.1+ds-1ubuntu4`
for `ubuntu:26.04`) so the same metadata + CLI produces the same ACPI bytes
regardless of when the regen runs. Pass an explicit version as the second
positional value of `--create-acpi-tables` to override the pin (e.g. to validate
an SRU), or `""` to opt into whatever `pull-lp-source` currently picks from the
main archive:

```
--create-acpi-tables ubuntu:26.04                              # default pin
--create-acpi-tables ubuntu:26.04 1:10.2.1+ds-1ubuntu5         # explicit version
--create-acpi-tables ubuntu:26.04 ""                           # current main-archive tip
```

### Indirect Boot

> **Supported distributions:** Ubuntu 24.04

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
    "qcow2": "[path to tdx-guest-ubuntu-24.04-generic.qcow2]",
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

The two crates (`tdx-measure` library at the repo root and `tdx-measure-cli` under `cli/`) form a single cargo workspace, so one command builds both and the binary lands in the shared `target/` directory:

```
cargo build --release
```

On success, the CLI build result is located at `target/release/tdx-measure`.

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
