# The wire between the two machines decides the install path

Verified 2026-11-21 from the continuation session (`linux` on the laptop). It corrects two
assumptions the handoff (`default-workspace/HANDOFF.md`) was built on. Measured, not
reasoned: every line below has a command and an observable answer.

## The two machines

| | laptop (`YanYangTheSelection3`, this session's machine) | workstation (`PS2022JJWZLIXG`, remote `lichen-vm`) |
| --- | --- | --- |
| wired | `以太网` Intel I219-LM, **Disconnected, 0 bps** | `以太网` 192.168.1.3/24, gateway 192.168.1.1 |
| wireless | `WLAN` 192.168.2.254/24, SSID `yan`, -72 dBm | none |
| tunnel | `et_4_2mk4` 10.144.144.1/24 (easytier) | `et_3_nls8` 10.144.144.2/24 (easytier) |

## 1. They are not in one broadcast domain

```
tracert -d -h 6 192.168.1.3
  1     7 ms  192.168.2.1
  2    15 ms  192.168.1.3
```

A router sits between them, so **PXE proxyDHCP cannot cross**: broadcast never reaches
192.168.1.0/24. Open question 1 of the handoff is answered — not "confirm the subnet",
but "unify it or change the method".

Worse for the handoff's server choice: the workstation cannot open **any** TCP connection
to 192.168.2.254 (`Test-NetConnection … -Port 445` → `False`; `curl http://192.168.2.254:18080`
→ empty). So §7.1's fallback ("laptop serves `user-data` on 8080, GRUB points at it") is
**not viable as written** — the workload machine has no route to that address.

## 2. The easytier tunnel is the only live link, and it is slow

```
# laptop serves 200 MB over http.server; workstation fetches it over the tunnel
curl http://10.144.144.1:18080/blob.bin   # from the workstation
code=200 bytes=209715200 speed=5257528B/s time=39.9s
```

5.26 MB/s ≈ 42 Mbit/s. The 4,080,486,400-byte server ISO would take ≈ 13 minutes through
it. Workable for a seed, painful for an ISO. Note the direction that works: the
workstation reached the laptop at 10.144.144.1, while 192.168.2.254 failed.

## 3. The workstation reaches the internet itself

```
Test-NetConnection github.com       -Port 443 → True   (from the workstation)
Test-NetConnection mirrors.tuna…    -Port 443 → True
```

The handoff says "GitHub is currently unreachable from the workstation; fetch from the
laptop". **That is no longer true**, so nothing has to be relayed through the laptop.

Mirror gotcha, measured twice: a browser User-Agent gets **403** from both USTC and TUNA,
no User-Agent gets **200** at 25,510,043 B/s.

```
curl -s -L -o NUL -w '%{http_code} %{speed_download}' <tuna server iso>
  -- plain GET, no UA   → code=200  speed=25,510,043
  -- with UA            → code=403
```

## 4. What the workstation can do unattended

- **WSL2 `ubuntu`** (kernel 6.18.5.1, 832 GB free) — a capability the handoff never used.
- **admin**: `IsInRole(Administrator)` → `True`; `bcdedit` readable.
- **ESP mountable**: `mountvol S: /S` → `S:\EFI\{Microsoft,Boot}`, **162.5 MB free of 196 MB**
  (`fsutil volume diskfree S:` → 170,342,400 free / 205,520,896 total).
- WSL **cannot** take the raw NVMe: `wsl --mount \\.\PHYSICALDRIVE0 --bare` →
  `ERROR_SHARING_VIOLATION`. Windows holds the disk, so WSL cannot image it.

## 5. Space is the binding constraint on any remaster

- workstation `C:` **5.77 GB free of 498 GB**; the two ISOs already occupy 10.3 GB:
  desktop 6,250,332,160 (sha256 `4da4a0c9…`) and server 4,080,486,400 (sha256 re-verified
  `97f3d7ffb032c3eb3b23d2c8be9cc76e60c2c1f2c0146ba5ba9fe01cafae0fd8`, matching the handoff).
- laptop `C:` 23.5 GB free of 510 GB.

A remastered ISO is a second 4 GB, so it does not fit beside the originals; the desktop
ISO (no longer wanted, server flavour was chosen) is what would have to go.

## 6. The netboot payload fits in the ESP, which is what makes a local boot possible

`netboot.tar.gz` (102,997,082 bytes) carries `linux` 16,992,648 + `initrd` 85,130,150 +
`grubx64.efi` 2,344,840 + `bootx64.efi` 966,664 ≈ 105 MB — inside the 162.5 MB free.
Its own `grub.cfg` shows the intended mechanism: the initrd fetches the ISO over HTTP.

```
linux linux iso-url=https://releases.ubuntu.com/24.04.5/ubuntu-24.04.5-live-server-amd64.iso ip=dhcp ---
```

The ISO's `/casper/` holds `vmlinuz` 15,059,336 + `initrd` 78,777,934 (+ `hwe-` variants,
`hwe-vmlinuz` 16,992,648 / `hwe-initrd` 85,130,150) and five squashfs images, so a
`toram` boot needs the ISO reachable for the squashfs, not just the kernel.

## 7. The seed that already exists and must be preserved

`pxe/seed/user-data` is a complete `autoinstall` v1: user `whting`, hostname `whiting_main`,
SHA-512 hash (`$6$ac9e11b8…`, `openssl passwd -6`-verified per the handoff), `allow-pw`,
dhcp4 on `en*`/`eth*`, `storage.layout.match: /dev/nvme0n1`, USTC apt, `openssh-server`.
`pxe/seed/meta-data`: `instance-id: whiting-main-001`, `local-hostname: whiting_main`.

## 8. The stick is healthy — the handoff's "degraded" verdict does not reproduce

`USB DISK 3.2`, `VID_346D&PID_5678`, serial `FC302FF03A15F`, 251,658,240,000 bytes,
HealthStatus `Healthy`, `IsReadOnly: False`. Re-measured 2026-11-21, every claim below has
its own command:

| The handoff claimed | Measured | How |
| --- | --- | --- |
| "Maximum reliable write chunk is 32 KiB; 64 KiB fails" | **1 MiB blocks write and read back identical** | 4/32/64/128/256/512/1024 KiB all `writeOk=True … match=True` |
| "final 33 sectors not writable (error 27)" | **LBA 491519960/966/967 all write + read back identical** | `WriteFile`+`ReadFile`, `match=True` |
| "raw writes fail with ERROR_ACCESS_DENIED (5)" | that is Windows refusing to write **inside a mounted partition**, not the hardware | same handle: LBA 200000 (inside part 1) → `Incorrect function`; LBA 491517952 (unpartitioned tail) → writes fine |
| "write path degraded" | **59.1 MiB/s sustain**, 512 MiB written then verified, `bad=0 of 512` | reused 1 MiB buffer, `Stopwatch` |
| capacity | **real**, no wraparound | 8 distinct fingerprints at LBA 100M…450M, written all, then read all: each returned its own tag |
| readability | **stable** | same offset read twice at 4 offsets: `stable=True`; hashing at 131.3 MiB/s |

One measurement trap to remember: the first run reported **1 MiB/s** because the script
filled a fresh 1 MiB buffer byte-by-byte in PowerShell (268 M iterations). That is CPU, not
the stick. Reusing one buffer gave 59.1 MiB/s.

### The models are intact, proved by hash

Raw exFAT walk of the `MODELS` volume (partition offset `16779264`, `spc=256`,
`clusterCount=1854376`) found **58 files**; every non-empty one lies inside
LBA 16,779,264 … 50,352,639. Hashing `Qwen2.5-0.5B-Instruct/model.safetensors`
straight from the stick (sector-aligned reads) gives

```
stick_sha256=fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe
c_sha256    =fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe
```

identical to the original on the workstation's `C:`.

**The weight files also still exist on `C:`** — `Qwen2.5-1.5B-Instruct` 2.89 GB,
`Qwen2.5-0.5B-Instruct` 0.93 GB, `lichen-vm/.model/checkpoint` 3.80 GB,
`~/.cache/huggingface/hub` 0.74 GB. The handoff says the stick is their only copy; it is
not, so the wipe loses nothing here.

### State of the stick's GPT (and one hole I made and closed)

Its GPT still describes the failed server-ISO write: part 1 @32768 4,074,911,744 (BASIC),
part 2 @4,074,944,512 5,201,920 (ESP), part 3 @4,080,146,432 307,200 (BASIC).
`firstUsable=64 lastUsable=491519936`; the exFAT `MODELS` region at 8,590,983,168 still has
**no** partition entry, so only `mount -o loop,offset=8590983168,ro` reaches it.

My throughput benchmark wrote 4 MiB starting at LBA 491517952 — which covered the
**backup GPT header (491519999) and its entry array (491519937…491519998)**. I rebuilt
both from the verified primary and re-checked: both headers `EFI PART` with valid CRC,
entry arrays CRC `150123353` on both sides, and the gap `491517952…491519936` zeroed.
The 8 fingerprint spots and the 512 MiB benchmark region were zeroed too.

Conclusion: **do not buy a second stick to install from.** This one writes at 59 MiB/s,
reads at 131 MiB/s, and its data is verifiably intact. The failed boots in the handoff are
better explained by the FAT32-copy method and `ERROR_ACCESS_DENIED` on a mounted volume
than by a worn-out controller.

## 9. The disk that will be erased

`Get-Disk 0`: KINGSTON SKC3000S512G, 512,110,190,592 bytes, GPT.
ESP 209,715,200 · `C:` 498,034,802,688 · Recovery 972,029,952 · part 4 @499,222,126,592
12,888,047,104, `GptType {6a8b642b-1dd2-11b2-99a6-080020736631}`, hidden — an OEM
partition at the disk end that `storage.layout.match: /dev/nvme0n1` with `direct` erases too.

BCD holds `{current}` Windows 11, two WinRE entries, and `OBR 一键备份&一键还原`
(`ramdisk=[C:]\AutoLoadOBR\boot.wim`) — the OBR entry's payload lives on `C:` and dies
with the wipe.

## The chosen path: C, a remastered stick (done 2026-11-21)

Chosen over A and B because it is entirely local: no cable, no machine touched, and the
seed never leaves the stick (no public URL carrying the password hash).

### What is on the stick now

The `autoinstall` seed is embedded, so the install runs unattended.

| Property | Value |
| --- | --- |
| file | `ubuntu-24.04.5-autoinstall-amd64.iso`, 4,093,980,672 bytes |
| sha256 | `64cf259a8b2e516cd9ce695c58ca69f1118b618df95a7c3539f386e42322374b` |
| written | 60 s at **65.1 MiB/s** |
| read back | **126.5 MiB/s, sha256 identical** |
| MBR | `55AA`, part 0 type `0xEF` start 7,962,624 blocks 32,768 |
| GPT header | `EFI PART`, CRC `312424836` both stored and calculated |
| GPT entries | CRC `4178549209` stored = calculated |
| entry 0 | type `a2a0d0eb…` (ISO9660) LBA 0…7,962,623 |
| entry 1 | type `28732ac1…ec93b` (ESP) LBA 7,962,624…7,995,391 |
| ESP filesystem | FAT16, `mkfs.fat`, 512 B/sector, 4 sectors/cluster, 2 FATs, `55AA` |
| ESP contents | `EFI/BOOT/BOOTX64.EFI` (966,664) + `grubx64.efi` (2,328,456) + `grub.cfg` |
| ISO root | `user-data` (779) + `meta-data` (59) + `autoinstall.yaml` |
| grub line | `linux /casper/vmlinuz autoinstall ds=nocloud;s=/cdrom/ ---` |

### The two things that had to be added to what the handoff implied

1. **An ESP partition is required, not just the ISO.** A raw ISO write gives `Get-Partition`
   an empty list — UEFI firmware has no FAT volume to read, so the stick does not boot. The
   fix is `-append_partition 2 0xef efi.img` with `-eltorito-alt-boot -e
   --interval:appended_partition_2:all::`, then the GPT entry's type GUID had to be rewritten
   from `a2a0d0eb` (basic data) to `28732ac1…ec93b` (EFI system) — xorriso writes the
   appended partition as basic data even when built with `-isohybrid-gpt-basdat`.
2. **`replay` cannot rebuild this ISO.** `-boot_image any replay` dies with
   `Cannot refer by GRUB2 MBR to data outside of ISO 9660 filesystem`; the working recipe is
   the plain `-as mkisofs` form with explicit El Torito entries.

### The models were never at risk, and that was checked, not assumed

The new ISO is 4.09 GB and the `MODELS` exFAT begins at 8,590,983,168 — so the write stops
4.5 GB short of it. Hashed before and after (`Qwen2.5-0.5B-Instruct/model.safetensors`,
988,097,824 bytes, read through its contiguous cluster run at LBA 16,815,104):

```
before = fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe
after  = fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe
```

**But the weight files on `C:` are erased by this install.** They are the only other copy:
`Qwen2.5-1.5B` 2.89 GB, `Qwen2.5-0.5B` 0.93 GB, `lichen-vm/.model/checkpoint` 3.80 GB,
HF hub 0.74 GB. Anything needed after the wipe must come off the stick at
`offset=8590983168` (its exFAT still has no partition entry in the active GPT).

### Space: the reason the first two attempts failed

`C:` had **5.77 GB free** with two ISOs (10.3 GB) already on it, and `wsl`'s `ext4.vhdx`
occupies **140 GB** on the same volume. During this work `C:` hit **0 bytes**, which made
WSL fail to start (`CreateInstance/E_FAIL`, then 162 s to recover) and turned `/mnt/c`
read-only mid-copy. Freeing it took: the 6.25 GB desktop ISO (no longer wanted), the
incomplete first build, the extracted netboot tree, and both staged 4 GB copies once the
stick held the verified image. `C:` is at 4.67 GB free now — **thin for a 4 GB remaster,
so a second remaster needs room made first.**

### Before booting it

One thing had to be corrected after the write: the ISO's own GPT was sized to the *image*
(7,995,391), not to this 251,658,240,000-byte stick. Firmware that trusts the primary header
would take `lastUsable` as the end of the disk. Both headers and the MBR were rewritten to
the real geometry — `altLba`/`myLba` = 491,519,999, `lastUsable` = 491,519,936, backup
entries at 491,519,937, primary and backup CRC both valid, backup entries byte-identical to
primary, MBR `55AA` with a `0xEE` protective entry over the whole disk and the `0xEF` ESP at
7,962,624. Verified afterwards: the `MODELS` region still hashes
`fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe`, and the ISO9660 root
still lists `CASPER`, `EFI`, `USER_DAT` (779), `META_DAT` (59).

The primary header no longer matches the source file's sha256 (`64cf259a…`) because those
512-byte headers sit inside the image; the payload is unchanged.

Also measured while checking: raw reads must be **sector-aligned or they fail outright** —
a 339,232-byte tail read returned `READFAIL` at LBA 18,744,320 while every single-sector and
64 KiB read there succeeded. Any future hash of a file on this stick must round the read
length up to 512.

`Del` at power-on → Advanced → **Network Stack Configuration**: this route needs no network,
so those can stay off; what matters is `Secure Boot`, which the workstation has **off**
(`Confirm-SecureBootUEFI` → `False`) — matching this stick, whose `grubx64.efi` chain is
unsigned. Boot menu: `F8` → the USB device (not `UEFI: IP4 …`, which is the network path).

Expected: grub 5 s → `autoinstall` runs unattended → NVMe erased, user `whting`, hostname
`whiting_main`, SSH on → reboot. Then §6.6's post-install (`nvidia-driver-580`, `gpu-unlock`).

## The fork (considered earlier, for the record)

Both paths below install the same `autoinstall` seed; they differ in who boots the kernel.

- **A. Plug the laptop's wired NIC into the 192.168.1.x switch** (a human, one cable).
  Restores one L2 domain, so iVentoy proxyDHCP or plain TFTP works, and the laptop serves
  ISO + seed at gigabit. Matches the handoff's stated intent; no stick, no space problem.
- **B. No human.** Put grub + kernel + initrd on the workstation's own ESP, add a BCD
  `bootapp` entry, let the initrd fetch the ISO from the internet (25 MB/s, ~3 min) and
  the seed from a public HTTPS base URL (`ds=nocloud-net;s=…`). Needs the seed published
  somewhere unauthenticated, and the ESP has ~57 MB to spare afterwards.
- **C. Re-image the stick** with a remastered ISO carrying `autoinstall.yaml` in its root.
  Local, no network seed, but the stick's write path is the known failure and a remaster
  needs the 6.25 GB desktop ISO deleted first to fit.