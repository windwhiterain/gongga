# Remastered stick boots to an installer error — what is known

Written before a context handoff. The `_archive` at `handoff`'s mentor can be read back with
`get_messages` when a detail is missing; this file is the state that matters.

## The user's goal

Replace Windows 11 on the workstation with Ubuntu 24.04 LTS server, unattended, then hand the
machine to an agent over SSH and enable full-rate FP16/BF16 tensor cores via
`Zagitta/gpu-unlock`. The original brief is
`C:\Users\Admin\Documents\deepseek-harness\default-workspace\HANDOFF.md` on the workstation
itself — **that file dies with the install**, and much of it is already known to be wrong
(see @file(.agents/notes/2026-11-21-the-wire-between-the-two-machines.md)).

Route chosen: **C — a remastered USB stick**, over (A) plugging a cable to merge the subnets and
(B) an ESP + network seed. Reason: it is entirely local, and the cloud-init seed (which carries
a password hash) never leaves the stick.

## Machines

- **laptop** `YanYangTheSelection3` — this session's machine. WLAN 192.168.2.254 behind a router
  at 192.168.2.1; wired NIC `以太网` disconnected. WSL2 distro `ubuntu` with ~816 GB free inside.
- **workstation** `PS2022JJWZLIXG`, remote name `lichen-vm`, wired 192.168.1.3. The box being
  reinstalled. **Unreachable since it left Windows** —
  `exec` answers `the peer did not say where its home is, so this build cannot place its payload
  there`. That is expected while it is in the installer; it is not a fault to fix.

## What is proven about the stick (do not re-litigate)

`USB DISK 3.2`, serial `FC302FF03A15F`, 251,658,240,000 bytes. The handoff's "degraded /
fake capacity" verdict **does not reproduce**; measurements are in the note above. Short form:
capacity real (8 distinct fingerprints to LBA 450M, no wraparound), write 65.1 MiB/s, read
126.5 MiB/s, `HealthStatus: Healthy`.

## What is on it now

| Item | Value |
| --- | --- |
| file written | `/root/usbwork/out/final.iso`, 4,093,980,672 bytes |
| sha256 as staged | `64cf259a8b2e516cd9ce695c58ca69f1118b618df95a7c3539f386e42322374b` |
| **sha256 now differs on purpose** | after writing, both GPT headers and the MBR were rewritten for full-disk geometry, so the first 1 MiB no longer matches the staged file. The ISO payload is unchanged. |
| partition 0 | ISO9660, LBA 0 … 7,962,623, root lists `CASPER`, `EFI`, `USER_DAT` (779), `META_DAT` (59) |
| partition 1 | ESP, type `28732ac1…ec93b`, LBA 7,962,624 … 7,995,391, FAT16 |
| ESP files | `EFI/BOOT/BOOTX64.EFI` (966,664), `grubx64.efi` (2,328,456), `grub.cfg` (447) |
| GPT | primary + backup both valid, `myLba`/`altLba` 491,519,999, `lastUsable` 491,519,936, backup entries at 491,519,937 |
| MBR | `55AA`, `0xEE` protective over the whole disk + `0xEF` ESP at 7,962,624 |
| ISO grub line | `linux /casper/vmlinuz autoinstall ds=nocloud;s=/cdrom/ ---` |

The ESP `grub.cfg` does not hard-code a root; it runs
`search --no-floppy --set=root --file /casper/vmlinuz`, then boots from there.

## Where the install stands

The stick **boots**: grub's menu appears, the kernel starts, the installer starts. It then fails
with

```
an error appened, press enter to open a shell
xxx finished
```

That text was read off the screen by the user; the `xxx` is a truncated program name, not a
transcription error to interpret. **We do not yet have the installer log**, so the cause is not
established. The user was offered three ways forward and has not yet answered: (A) read the logs
at the shell, (B) fall back to a manual install, (C) guess the most likely seed fault and rebuild.

### Unconfirmed suspects, in the order I would test them

1. **`hostname: whiting_main`** in the seed — an underscore is not legal in a hostname. This
   value was inherited verbatim from the old handoff and never questioned until now. Most likely
   single cause. Fix would be `whiting-main`.
2. **`packages: [openssh-server]`** with `apt:` pointing at a mirror — this needs the network,
   and this route was chosen precisely so no network is required. If the live environment has no
   route, apt fails. Fix would be to drop `packages:` and install `openssh-server` afterwards.
3. **`storage.layout.match.path: /dev/nvme0n1`** — support for a `path` key under `match` varies
   between subiquity versions.

The seed that was embedded is at
`C:\Users\Admin\Documents\deepseek-harness\default-workspace\pxe\seed\user-data` on the
workstation (readable only while that machine runs Windows) and at `/root/usbwork/cidata/` and
`/root/usbwork/iso/` inside the laptop's WSL.

## Rebuild recipe that works (each step cost a failure to learn)

Inside WSL `ubuntu` as root:

```
W=/root/usbwork                      # extracted ISO tree lives at $W/iso
cd "$W/iso"
xorriso -as mkisofs -r -V UBUNTU_AUTOINSTALL -J -joliet-long \
  -b /boot/grub/i386-pc/eltorito.img -c /boot.catalog -no-emul-boot \
  -boot-load-size 4 -boot-info-table \
  -eltorito-alt-boot -e --interval:appended_partition_1:all:: -no-emul-boot \
  -append_partition 1 0xef "$W/efi.img" \
  -isohybrid-gpt-basdat -isohybrid-mbr /usr/lib/grub/i386-pc/boot_hybrid.img \
  -o "$W/out/u2.iso" "$W/iso"
```

Then the appended partition's GPT type must be rewritten from `a2a0d0eb…` (basic data) to
`28732ac1…ec93b` (EFI system) in **both** the primary and backup entry arrays, and both header
CRCs recomputed — xorriso labels it basic data even with `-isohybrid-gpt-basdat`. Then, after
writing to the stick, rewrite MBR + both GPT headers for the real disk geometry as described
above.

Traps that cost time, worth not repeating:

- `xorriso -boot_image any replay` **cannot** rebuild this ISO: `Cannot refer by GRUB2 MBR to
  data outside of ISO 9660 filesystem`.
- Raw device reads **must be 512-byte aligned** or they fail outright. A 339,232-byte tail read
  returned `READFAIL` at LBA 18,744,320 while every sector-sized and 64 KiB read there succeeded.
- In PowerShell, a function named `RD` collides with the `rd`/`Remove-Item` alias; and `1u` /
  `2147483648u` suffixes are C#, not PowerShell.
- `Add-Type` C# types do not survive between `exec` calls — each call is a new process. Write the
  script to a file and run it.
- Do not copy 4 GB into `C:` without checking free space first; see below.

## Disk-space hazard on the workstation

- `C:` was **0 bytes free** at one point, which made WSL fail to start
  (`CreateInstance/E_FAIL`, 162 s to recover) and turned `/mnt/c` read-only in the middle of a
  copy. Freed by deleting the 6.25 GB desktop ISO, the first failed build, the netboot tree and
  both staged 4 GB copies.
- `C:` is at **4.73 GB free now**, and WSL's `ext4.vhdx` is **140 GB, on `C:`**.
- Therefore: **any second remaster needs room made first, and the staged ISO must be deleted
  again before the next one is staged.**

## Data that dies with the install (accepted)

- Weight files on `C:`: `Qwen2.5-1.5B` 2.89 GB, `Qwen2.5-0.5B` 0.93 GB,
  `lichen-vm/.model/checkpoint` 3.80 GB, HF hub 0.74 GB. The stick holds a verified copy of the
  same files at byte offset **8,590,983,168** (exFAT volume `MODELS`, still with no partition
  entry — reach it with `mount -o loop,offset=8590983168,ro`). `Qwen2.5-0.5B-Instruct/
  model.safetensors` hashed identical from stick and from `C:`
  (`fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe`), and re-hashed identical
  after the final write. **Copy the models off the stick before anything else reformats it.**
- `C:\Users\Admin\Documents\deepseek-harness\default-workspace\` — the whole old handoff
  workspace, including `write-usb.ps1`, `dump-gpt.ps1`, `read-sectors.ps1` and the seed files.
  Useful still; all on the doomed disk.
- Repos are fine: `gongga`, `whiting`, `rusd` clean and pushed; `lichen-vm` has only an untracked
  `.scratch-core/`. `rusd`'s 125 GB is `.wt/` + `target/`, both gitignored and rebuildable.

## Environment rules that apply here

- Builds go through `mbx <cargo subcommand>`, never bare `cargo`.
- Nothing in this session's notes directory has been pushed; the note above is committed locally
  on `main` as `62d42dfe`.