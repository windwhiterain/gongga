# Recycle bin: the 0 limit works; what bypasses it is a program, not the shell

Asked why 51 GB could sit in `C:\$Recycle.Bin` when the user had set the bin
limit to 0. Measured on the laptop (`YANYANGTHESELEC`, volume
`{352c2d27-9cbb-4a0b-9696-b57b3e467bcb}` = C:).

## The setting was in force all along

```
MaxCapacity=0   NukeOnDelete=1     (C:, per-user BitBucket key)
```

Deleting a 1 MB and a 2 MB file through `Shell.Application`'s delete verb left
the bin unchanged at `1 file / 129 bytes`, and that one file is `desktop.ini`,
not recycled payload. The files were gone permanently. So the shell path honours
the setting; my first reading (a 129-byte bin entry looked like a recycled file)
was wrong and the correction matters.

## What actually filled it

87,700 files, **51.00 GB**, all `$R*.rlib` (Rust static libraries), written in
bursts on 11-21 and 11-22 11:27–11:31, all under the user's own SID
(`...-1001`). That is a build-cleanup step moving target directories into the
bin by writing `$I`/`$R` pairs itself instead of asking the shell — which is
exactly the case a per-user bin preference cannot govern.

The `$I` metadata that names the original paths was lost when the bin was
emptied, so the tool is not identified. If it recurs, read `$R`/`$I` **before**
clearing.

## Hardening: policy yes, ACL no

Applied and kept: `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\Explorer`
`NoRecycleFiles=1` (DWORD), which applies the "delete permanently" behaviour to
every program that goes through the shell.

Attempted: deny `CreateFiles|AppendData|ChangePermissions` (later also
`Delete|DeleteSubdirectoriesAndFiles`) for the user, SYSTEM and Administrators on
`C:\$Recycle.Bin\<SID>`. It works immediately — a direct write is refused — but
**is undone the next time the shell deletes anything**: the folder's ACL returns
to its three inherited `Allow FullControl` entries. Reproduced three times,
including with `Delete` and `ChangePermissions` denied to every principal that
could plausibly rewrite it, so some principal with WRITE_DAC remains. Not
pursued further; do not describe this as protection.

## Freed

Recycle bin emptied with the robocopy empty-mirror method (`Clear-RecycleBin`
hangs at this file count): C: went from **6.55 GB to 40.28 GB** free, 33.7 GB
released, 0 files left.

## If it comes back

The durable fix is the deleting tool: make it remove target directories directly
instead of recycling them. Until then a periodic empty is the reliable measure,
and `NoRecycleFiles=1` at least keeps Explorer's own deletes out of the bin.
