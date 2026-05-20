#!/usr/bin/env bash
# Fetch OVMF.fd into tests/fixtures/ for the test harness to use. Pinned to the
# Ubuntu 25.04 edk2 package so the bytes match across machines and CI runs.
#
# Idempotent: skips the download if the file is already present with the
# expected sha256.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/fixtures/OVMF.fd"
expected_sha256=9e807cb2cd4313406a3aa4becc0836671a5c64ca7bdc08a45e15260184b446bf

if [[ -f "$out" ]] \
   && [[ "$(sha256sum "$out" | awk '{print $1}')" == "$expected_sha256" ]]; then
    echo "$out already present and matches expected sha256"
    exit 0
fi

mkdir -p "$here/fixtures"
tmp_deb="$(mktemp --suffix=.deb)"
trap 'rm -rf "$tmp_deb" "${tmp_deb%.deb}.d"' EXIT

wget -q http://archive.ubuntu.com/ubuntu/pool/main/e/edk2/ovmf_2025.02-3ubuntu2_all.deb -O "$tmp_deb"
extract_dir="${tmp_deb%.deb}.d"
dpkg-deb -R "$tmp_deb" "$extract_dir"
cp "$extract_dir/usr/share/ovmf/OVMF.fd" "$out"

actual="$(sha256sum "$out" | awk '{print $1}')"
if [[ "$actual" != "$expected_sha256" ]]; then
    echo "OVMF.fd sha256 mismatch: got $actual, expected $expected_sha256" >&2
    exit 1
fi
echo "$out ready (sha256=$expected_sha256)"
