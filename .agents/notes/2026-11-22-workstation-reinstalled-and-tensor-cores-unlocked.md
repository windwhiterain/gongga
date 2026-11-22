# Workstation reinstalled; tensor cores unlocked and measured

Final state of the job started in
`.agents/notes/2026-11-21-install-blocked-at-installer-error.md` and continued in
`.agents/notes/2026-11-21-seed-patched-in-place-on-the-stick.md`.

## The machine now

- `whiting-main`, Ubuntu 24.04.5 LTS, kernel 6.8.0-151-generic, user `whting`
  (password is the one the user holds; the seed only ever carried its hash).
- `enp5s0` = **192.168.1.3/24** (DHCP). SSH reachable from the laptop at
  192.168.2.254 — `Test-NetConnection 192.168.1.3 -Port 22` succeeds; the old
  gongga channel `lichen-vm` is dead with the wiped Windows disk.
- `/dev/nvme0n1`: 1 GiB EFI + 475.9 GiB `/`, 431 GiB free. USB stick shows as
  `sda` with a 0B size report; harmless.
- GPU: NVIDIA GA104 **RTX 3060 Ti LHR** (10de:2489), driver **580.178.04**,
  CUDA 13.0, 8192 MiB.

## How the stick was fixed (no remaster, no GRUB edit)

Patched the three seed extents in place on `\\.\PhysicalDrive1` of the Windows
workstation, keeping each file's length identical so no ISO9660/Joliet directory
record, GPT entry or grub.cfg byte changed: LBA 615 (`autoinstall.yaml`),
1989119 (`user-data`), 939775 (`meta-data`), all in 2048-byte units.
Corrected 779-byte seed: `.dev/seed/user-data.779`, sha256
`1f0700f9e2827a6d56851e46a40c0deaa34584fd2785d461b7c3bc355e5fdd7c`.
Original bytes were hashed before every write and the write aborted on mismatch;
readback from a fresh handle matched. The install then ran unattended from the
unmodified GRUB entry. `md5sum.txt` still lists old hashes — only matters if the
`verify` boot option is ever added.

## gpu-unlock 2.0.0

The box cannot reach GitHub (TLS handshake times out); PyPI and the Ubuntu
mirrors work. So the repo and the two NVIDIA inputs (610.43.03 source tarball
26 MB + installer 344 MB) were fetched on the laptop through the proxy and
uploaded by SFTP. SFTP drops the executable bit — `find . -name '*.sh' -exec
chmod +x {} +` was needed before `make deb` (`packaging/snapshot.sh` feeds
`git archive`).

On the box: `make deb INPUTS=/home/whting/gpu-unlock-inputs`, then installed
`gpu-unlock-dkms` + `gpu-unlock-firmware`, then `gpu_unlock.targets=all` in
`GRUB_CMDLINE_LINUX_DEFAULT` + `update-grub`. DKMS builds it for each kernel
with headers; the initramfs hook loads it before `nvidia`.

## Measured, same card, same script

FP16 GEMM with FP32 accumulation, n=8192, 20 reps, CuPy 14.2.0
(`/home/whting/gemm_bench.py`):

| state | TFLOPS |
| --- | --- |
| unlock off (cmdline empty, `unlocked` absent from dmesg) | 37.6, 37.6, 37.5 |
| unlock on | 72.8, 74.1, 74.1 |
| unlock on, after re-enabling at the end | 70.9, 72.9 |

**≈1.97×**. Matches the README's 3060 Ti reference (37 before / 68 after).
Final boot state: `gpu_unlock.targets=all`, dmesg `gpu_unlock: unlocked`,
`ss0=0x88888888 ss1=0x8`, handoff `ret=0`.

## Tooling left for the next session

- Laptop `.dev/ssh/`: `run.py` (run a remote command, `--sudo` pipes the
  password), `push.py` (recursive SFTP upload), plus a paramiko venv.
  Always `export MSYS_NO_PATHCONV=1` first: Git Bash rewrites arguments that
  start with `/` into `C:/SoftWare/git/Git/...`, which silently broke the first
  upload.
- Box `/home/whting/`: `gpu-unlock/` source + build tree, `gpu-unlock-inputs/`,
  `bench/` venv, `gemm_bench.py`, `toggle_unlock.sh on|off` (edits grub,
  update-grub, schedules a reboot).

## Not done

No model is deployed and nothing serves inference yet; the stick's model copy
was waived by the user. If models are wanted later they must be re-downloaded.
