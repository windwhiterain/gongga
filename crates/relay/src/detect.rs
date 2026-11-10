//! Which program a name runs here, asked the way the server asks it.
//!
//! # Invariant
//!
//! Resolved in this process's environment, because this process *is* the machine the
//! program runs on: a relay on a peer answers for that peer. A bare `PATH` name is not
//! enough on Windows, where `python3` is often a Store stub and `bash` is Git's.

use std::path::PathBuf;

pub fn git_bash() -> Option<PathBuf> {
    if cfg!(windows) {
        let candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
            r"C:\SoftWare\git\Git\bin\bash.exe",
        ];
        for c in candidates {
            let p = PathBuf::from(c);
            if p.exists() {
                return Some(shell_program(p));
            }
        }
    }
    which::which("bash").ok().map(shell_program)
}

pub fn shell_program(program: PathBuf) -> PathBuf {
    let Some(name) = program.file_name() else {
        return program;
    };
    let Some(bin) = program.parent() else {
        return program;
    };
    if bin.file_name().and_then(|n| n.to_str()) != Some("bin") {
        return program;
    }
    let Some(root) = bin.parent() else {
        return program;
    };
    let real = root.join("usr").join("bin").join(name);
    if real.is_file() { real } else { program }
}

pub fn python() -> Option<PathBuf> {
    for name in ["python3", "python"] {
        let Ok(candidate) = which::which(name) else {
            continue;
        };
        let Ok(out) = std::process::Command::new(&candidate).arg("-V").output() else {
            continue;
        };
        let said = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        if out.status.success() && said.starts_with("Python") {
            return Some(candidate);
        }
    }
    None
}
