#!/usr/bin/env python3
"""A stand-in for sftp's batch mode: `put`, `get` and `chmod` against the local disk."""
import os
import shlex
import shutil
import stat
import sys

if __name__ == "__main__":
    for line in sys.stdin:
        parts = shlex.split(line.strip())
        if not parts:
            continue
        verb = parts[0]
        if verb in ("put", "get") and len(parts) >= 3:
            source, target = parts[1], parts[2]
            try:
                shutil.copyfile(source, target)
            except OSError as error:
                sys.stderr.write("fake sftp: %s\n" % error)
                sys.exit(1)
        elif verb == "chmod" and len(parts) >= 3:
            mode = int(parts[1], 8)
            os.chmod(parts[2], mode)
        elif verb in ("quit", "bye"):
            break
        else:
            sys.stderr.write("fake sftp: cannot do %r\n" % (parts,))
            sys.exit(1)
