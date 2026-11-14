//! What a skill runs as: a Python package, and this home's contract.
//!
//! See `docs/notes/skill-package.md`.

use std::path::{Path, PathBuf};

/// The file that makes a skill directory a package.
const PACKAGE: &str = "__init__.py";

/// The contract a package's runner is: the content's `python` tree, installed under
/// this home's `lib/`.
const CONTRACT: &str = "gongga_skill.py";

/// Whether a skill directory is a Python package: the file that makes one.
///
/// # Invariant
///
/// The one place that names `__init__.py`: the server refuses a directory without it, and
/// nothing else decides what a skill runs as.
pub fn is_package(directory: &Path) -> bool {
    directory.join(PACKAGE).is_file()
}

/// What starts a package skill: this home's interpreter, and the contract it reads.
#[derive(Debug, Clone)]
pub struct Package {
    /// The interpreter, as the caller's config resolved it.
    pub python: PathBuf,
    /// This home's `lib/gongga_skill.py`, which reads the call.
    pub contract: PathBuf,
}

/// A package skill's runner: this home's contract file reads the call off stdin.
///
/// # Invariant
///
/// The one form a skill runs as. `python` is the interpreter the caller resolved, since
/// this crate looks none up, and `lib` is this home's: the content installs the contract
/// there, so changing it is never a rebuild.
pub fn package_of(python: Option<&Path>, lib: Option<&Path>) -> Result<Package, String> {
    let Some(lib) = lib else {
        return Err(
            "this home has no `lib` directory, so a package skill has no contract to run: \
             the content's `python` tree lands there at startup"
                .to_string(),
        );
    };
    let contract = lib.join(CONTRACT);
    if !contract.is_file() {
        return Err(format!(
            "{} is not there: this home's content install did not place the skill contract",
            contract.display()
        ));
    }
    let Some(python) = python else {
        return Err(
            "a package skill is Python and no Python was found: install one, or name it in \
             the config (`GONGGA_PYTHON`, `[tools.python] program`)"
                .to_string(),
        );
    };
    Ok(Package {
        python: python.to_path_buf(),
        contract,
    })
}

/// The one line a package's runner reads on stdin.
///
/// # Invariant
///
/// Three keys and no command line: the skill's directory, the function it publishes, and
/// the arguments as an object of named ones. Written here so a caller cannot spell the
/// bytes differently; `args` is `{}` when a call names none.
pub fn request(directory: &Path, function: &str, args: &serde_json::Value) -> String {
    let args = match args {
        serde_json::Value::Null => serde_json::Value::Object(serde_json::Map::new()),
        other => other.clone(),
    };
    let mut line = serde_json::json!({
        "skill": directory.display().to_string(),
        "function": function,
        "args": args,
    })
    .to_string();
    line.push('\n');
    line
}
