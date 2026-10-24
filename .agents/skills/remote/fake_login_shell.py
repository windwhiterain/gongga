#!/usr/bin/env python3
"""A stand-in for sshd that runs the command locally in a POSIX login shell.

The interesting properties of the real thing -- a shell that is handed a command string
and a channel for stdin/stdout -- are all that a session depends on. Reproducing those
two facts locally makes the session testable without a network, and without the
Windows/PowerShell shape that hides the POSIX path.
"""
import subprocess
import sys

if __name__ == "__main__":
    command = sys.argv[-1]
    shell = "bash"
    sys.exit(subprocess.run([shell, "-c", command]).returncode)
