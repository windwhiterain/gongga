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
An error occurred. Press enter to start a shell
```

The cause is settled and is a defect in the seed, not in the stick: the `apt:` block's entries
carry no `arches` key, and curtin's `get_arch_mirrorconfig` iterates `None`. See
"The cause, settled" below for the reproduction and the fixed block.

The user was offered three ways forward and has now been shown the error screen itself: they are
at the installer shell, so **reading the log is one command away** and rebuilding is not needed
to learn anything more.

### The cause, settled

The user photographed the screen, and the photo closes this: subiquity did **not** reject the
seed. It crashed inside its Mirror controller.

```
finish: subiquity/Mirror/apply_autoinstall_config: argument of type 'NoneType' is not iterable
finish: subiquity/apply_autoinstall_config: argument of type 'NoneType' is not iterable
```

`argument of type 'NoneType' is not iterable` is Python's TypeError for `x in None`. The seed
carries the legacy mirror form, with no `arches` on either entry:

```yaml
  apt:
    geoip: false
    primary:
      - uri: http://mirrors.ustc.edu.cn/ubuntu/
    security:
      - uri: http://mirrors.ustc.edu.cn/ubuntu/
```

subiquity wraps that list in one `LegacyPrimaryEntry` and reads its `uri` through curtin's
`get_arch_mirrorconfig` (`curtin/commands/apt_config.py:841`), which does

```python
for mirror_cfg_elem in mirror_cfg_list:
    arches = mirror_cfg_elem.get("arches")   # None: the seed wrote no arches
    if arch in arches:                       # -> TypeError
```

Reproduced twice, at two different depths. First by copying that loop into a script and feeding it
the seed's own list: the message is byte-for-byte `argument of type 'NoneType' is not iterable`.
Then against the real thing — subiquity `24.04.5`'s own `subiquity/models/mirror.py`, imported
from the tag with curtin stubbed at the four names it imports, and driven as the installer drives
it:

```
user-data.orig : TypeError: argument of type 'NoneType' is not iterable
user-data.fixed: OK  legacy=True uris=['http://mirrors.ustc.edu.cn/ubuntu/']
```

It surfaces at `apply_autoinstall_config` rather than at load because `geoip: false` short-circuits
`wants_geoip` (the other reader of `uri`), and `find_and_elect_candidate_mirror` reads
`candidate.uri` only after `has_network` is true — which the wired NIC's DHCP makes it.

Version pinned: the prompt in the photo is `/var/snap/subiquity/7424`, and revision 7424 on the
`24.04` track is subiquity **24.04.5** (`api.snapcraft.io` channel-map). The code quoted is the
`24.04.5` tag.

This clears the three old suspects: `hostname: whiting_main` is never validated by subiquity
(`IdentityController` checks only the username), and `storage.layout.match.path` is a supported
`MatchDirective` key in 24.04.5. `packages: [openssh-server]` is not the cause either, though it
is a second thing that needs the archive to answer.

### The fix

```yaml
  apt:
    geoip: false
    primary:
      - arches: [default]
        uri: http://mirrors.ustc.edu.cn/ubuntu/
    security:
      - arches: [default]
        uri: http://mirrors.ustc.edu.cn/ubuntu/
```

Also drop `packages: [openssh-server]`: `ssh.install-server: true` in the same seed already
installs it. And `hostname: whiting_main` → `whiting-main` (in `meta-data`'s `local-hostname`
too) — not the cause, but the underscore is not a legal host name. Keep
`storage.layout.match.path`: `path` is a supported 
`MatchDirective` key in 24.04.5.

### The corrected seed, byte for byte

The corrected `user-data` is **798 bytes**, sha256
`6885c84ab6bf36278bfb580827a07a0dbcb1756bd5657e6539179a8f62107764`. Two independent builds agree
byte for byte: one typed from scratch, one derived from the original 779-byte text recovered from
the transcript (LF endings; the stored copy is 815 bytes because it was captured through CRLF).
The corrected `meta-data` is **59 bytes**, sha256
`19996dc824c72997f19b218a5137c6ca8d6a461f623cae87d65c8fadac018dd4`:

```yaml
instance-id: whiting-main-001
local-hostname: whiting-main
```

Both sit in `.dev/seed/` in this checkout as `user-data.fixed` and `meta-data.fixed` (untracked,
local), with `check.py` to re-verify them; the original is `user-data.orig` (815 bytes as stored,
779 with LF) for diffing. The same corrected `user-data` is reproduced in full in the patch block
below, so the note stands on its own.

The seed text itself was recovered from the transcript, not retyped from memory: it is row 207 of
`linux/mentor` (`get_messages`, the `seed-read` tool answer), and the same file appeared again at
rows 495 and 523.

The seed that was embedded is at
`C:\Users\Admin\Documents\deepseek-harness\default-workspace\pxe\seed\user-data` on the
workstation (readable only while that machine runs Windows). The copies that exist **now** are on
the stick itself — `USER_DAT` is ISO9660 partition 779 in the volume root, `META_DAT` is 59 — which
the installer can still read from its own shell.

### Getting the corrected seed read: three routes, in order of cost

The seed lives in the **ISO9660** root of the stick (`/cdrom/user-data`), and Linux mounts iso9660
read-only. So editing it in place normally cannot work — check with `mount | grep cdrom` at the
installer shell before assuming it can.

**Route 1 — a second stick labelled `CIDATA`, and one GRUB line edited by hand.** Cheapest, needs
no ISO tooling and no laptop WSL.

1. Any FAT-formatted stick (the broken 251 GB one can stay as it is) gets `user-data` and
   `meta-data` at its root, with the corrected bytes below, and the volume label set to `CIDATA`.
2. Boot the existing stick. At the GRUB menu press `e` on the entry and change the kernel line:
   `autoinstall ds=nocloud;s=/cdrom/` → `autoinstall` alone. With no `ds=` seedfrom, cloud-init
   scans for `LABEL=CIDATA` (`DataSourceNoCloud`, `find_devs_with("LABEL=cidata")`) and finds the
   second stick. `Ctrl-X` boots it.
3. The installer then reads the corrected seed and the crash cannot happen.

Cost: one FAT stick and two GRUB keystrokes. Risk: the `ds=` edit is at the bootloader, so a typo
leaves the machine booting without a seed — recoverable by power-cycling to the menu again.

**Route 2 — patch `/cdrom` if it happens to be writable.** At the shell: `touch /cdrom/x`. If that
succeeds, write the two files there, `sync`, and reboot the stick. Expected to fail on iso9660;
the measurement decides.

**Route 3 — remaster.** Needs WSL and xorriso re-installed on the laptop (both are gone) and about
10 GB of scratch on a `C:` with 23 GB free, which is why it is last.

### The corrected bytes, as files to write

Route 1 writes these onto the `CIDATA` stick's root; Route 2 writes them into `/cdrom`; Route 3
puts them in the extracted ISO tree. `wc -c` must print **798** and **59**.

```sh
cat > meta-data <<'EOF'
instance-id: whiting-main-001
local-hostname: whiting-main
EOF
cat > user-data <<'EOF'
#cloud-config
autoinstall:
  version: 1
  locale: en_US.UTF-8
  keyboard:
    layout: us
  identity:
    hostname: whiting-main
    username: whting
    realname: whting
    password: "$6$ac9e11b85376b0e4$WOkDJJOFtc8Cih1M/5Ejqm9lgPyd1x2vNUfVBKzSa4bIk3Y.CHWpVfNi08Z9/M0g02dEKoveIXk5nwzZpSn2X0"
  ssh:
    install-server: true
    allow-pw: true
  network:
    version: 2
    ethernets:
      wired_en:
        match: {name: "en*"}
        dhcp4: true
      wired_eth:
        match: {name: "eth*"}
        dhcp4: true
  storage:
    layout:
      name: direct
      match:
        path: /dev/nvme0n1
  apt:
    geoip: false
    primary:
      - arches: [default]
        uri: http://mirrors.ustc.edu.cn/ubuntu/
    security:
      - arches: [default]
        uri: http://mirrors.ustc.edu.cn/ubuntu/
EOF
wc -c user-data meta-data   # 798 and 59
```

On the FAT `CIDATA` stick the label is what matters, not the paths:
`fatlabel /dev/sdY1 CIDATA` (or Windows `label X: CIDATA`), then copy the two files to its root.

`--check` of the corrected pair, for the next agent: `python .dev/seed/check.py <user-data>` prints
`OK` (exit 0) or `FAIL` with each problem (exit 1). It replays curtin's own
`get_arch_mirrorconfig`, so it fails on the original seed with the installer's exact message:

```
$ python .dev/seed/check.py .dev/seed/user-data.orig   # FAIL, exit 1, four problems
$ python .dev/seed/check.py .dev/seed/user-data.fixed  # OK, exit 0
```

## What this laptop can and cannot do any more

The rebuild recipe below assumed the laptop's WSL. **WSL is gone from it**: `wsl -l -q` prints no
distro, `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss` does not exist, there is no
`ext4.vhdx` on the volume, `HyperVisorPresent` is `False`, and `xorriso`/`genisoimage`/`mkisofs`
are not installed. A remaster therefore cannot be made here without first re-installing WSL and
its toolchain, and the staged `/root/usbwork` tree described below no longer exists. Prefer the
routes above it: they need no ISO tooling and no second 4 GB stage on a `C:` with 23 GB free.

## Rebuild recipe that works (each step cost a failure to learn)

Inside WSL `ubuntu` as root (WSL must be installed first; see above):

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