# Seed repaired in place on the stick — state after the remote patch

Continuation of `.agents/notes/2026-11-21-install-blocked-at-installer-error.md`.
The install is still not run. This note records what changed since.

## Decision taken

User waived the model copy at byte offset 8,590,983,168 (was the reason a second
stick was proposed). The stick stays the only install medium. No second stick, no
ISO remaster, no GRUB keystrokes.

User brought the workstation back into Windows so the work could be done remotely:
`lichen-vm` = `PS2022JJWZLIXG`, ASUS physical board, Win11 Home, `admin=True`.
That box has WSL `ubuntu` alive (`wsl -d ubuntu -e echo alive` → alive, WSL
3.0.5.0); its `python313.exe` is broken (`PYTHONHOME` unset but prefix resolves to
`C:\Users\Admin`, `encodings` missing) — do not rely on it, use PowerShell.

## What was done (all writes are to \\.\PhysicalDrive1 on lichen-vm)

The seed lives inside the ISO9660 area as plain file data. Both the ISO9660 tree
and the Joliet tree point at the same three extents, and the corrected seed was
made **the same length** as the original, so no directory record, GPT entry or
grub.cfg byte needed changing.

| file | extent (2048-byte LBA) | byte offset | size |
| --- | --- | --- | --- |
| `AUTOINST.YAM` / `autoinstall.yaml` | 615 | 1,259,520 | 779 |
| `USER_DAT` / `user-data` | 1,989,119 | 4,073,715,712 | 779 |
| `META_DAT` / `meta-data` | 939,775 | 1,924,659,200 | 59 |

Original bytes (verified before every write; a mismatch would abort):
779-byte seed sha256 `edbc6ad614968b02fb0a8ae28dc9fcc960ed33215d33aa92d82480da9861073e`,
59-byte meta-data sha256 `bd91047dacdd0055a66813c7cb4476ecd51dfe7f883e7eaf27c2e67301fdc90f`.
Backups at `C:\Users\Admin\usbwork\backup\` on the workstation (dies with the install).

Write result, read back from a fresh handle:

```
autoinstall.yaml 1f0700f9e2827a6d56851e46a40c0deaa34584fd2785d461b7c3bc355e5fdd7c
user-data        1f0700f9e2827a6d56851e46a40c0deaa34584fd2785d461b7c3bc355e5fdd7c
meta-data        19996dc824c72997f19b218a5137c6ca8d6a461f623cae87d65c8fadac018dd4
```

## The 779-byte corrected seed

`.dev/seed/user-data.779` on the laptop, sha256 above. Same content as
`user-data.fixed` (798 bytes) but expressed with one YAML anchor so both mirror
entries share it, plus a trailing `#....` comment line padding to exactly 779:

```yaml
  apt:
    geoip: false
    primary:
      - &mirror {arches: [default], uri: http://mirrors.ustc.edu.cn/ubuntu/}
    security:
      - *mirror
```

`hostname: whiting-main`, `packages:` dropped (ssh.install-server installs it).
`python .dev/seed/check.py .dev/seed/user-data.779` → OK exit 0.

## Open risk

`md5sum.txt` (extent 939755) still hashes the old file contents. Casper only runs
that check when the `verify` boot option is given, so an unattended boot is not
affected; do not add `verify`.

## Next

Boot the stick: `F8` → the USB device (not `UEFI: IP4 …`). No GRUB edit any more —
the menu still reads `linux /casper/vmlinuz autoinstall ds=nocloud;s=/cdrom/ ---`
and now finds a correct seed. The install erases `/dev/nvme0n1` (Kingston 512 GB).
Login after install: user `whting`; only the password hash is on record, so the
plaintext password must come from the user or be reset in recovery.
Then `nvidia-driver-580` + `Zagitta/gpu-unlock` per the original brief.
