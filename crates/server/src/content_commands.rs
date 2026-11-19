//! The commands the content declares, published by this server.
//!
//! See `docs/notes/server-content-commands.md`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use tokio::sync::Mutex;

use protocol::{ArgSpec, CommandSpec, CommandTarget, CompletionSource};

use crate::tools::ToolRegistry;
use crate::tools::skill::Skills;
use crate::tools::spec::{Argument as Row, ToolSpec};

/// How long a package may take to answer what it publishes.
///
/// # Invariant
///
/// A hang guard, not a budget: a greeting is built while a client waits for it, and a
/// package that read stdin and never answered would hold that greeting forever.
const MANIFEST_DEADLINE: Duration = Duration::from_secs(10);

/// What the content declares, and what the last read of it answered.
pub struct ContentCommands {
    /// The home whose content declares them.
    home: PathBuf,
    state: Mutex<State>,
}

/// What one publish reads besides the content itself.
pub struct Publish<'a> {
    /// The interpreter and this home's contract, which a manifest request runs under.
    pub skills: Option<&'a Skills>,
    /// The rows: what an argument of a published name is called on an operator's line.
    pub tools: &'a ToolRegistry,
    /// The names this server already answers, which a declaration may not take.
    pub reserved: &'a [String],
}

/// One declaration with the arguments its function answered with.
#[derive(Debug, Clone)]
struct Manifest {
    name: String,
    skill: String,
    function: String,
    arguments: Vec<Signature>,
}

/// One argument of one declared function, as the contract answered it.
#[derive(Debug, Clone)]
struct Signature {
    name: String,
    required: bool,
    /// The JSON Schema type the annotation named; `None` is no type stated.
    type_of: Option<String>,
}

/// What a published name runs: the function, and the skill that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub skill: String,
    pub function: String,
}

#[derive(Default)]
struct State {
    /// What was read last; `None` until something was.
    fingerprint: Option<Fingerprint>,
    manifests: Vec<Manifest>,
    /// One line per declaration that could not be read, kept for the diagnosis.
    problems: Vec<String>,
    /// The call each published name runs, filled by the last [`ContentCommands::commands`].
    routes: HashMap<String, Route>,
}

/// What moves when the content has moved: the commit, its declarations, and each
/// declared package's one file.
#[derive(PartialEq, Eq, Clone)]
struct Fingerprint {
    commit: String,
    declarations: Vec<(String, String, String)>,
    packages: Vec<PackageFile>,
}

/// A package as it stood when it was last read: a local edit re-asks what it publishes.
#[derive(PartialEq, Eq, Clone)]
struct PackageFile {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

impl ContentCommands {
    pub fn new(home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            home,
            state: Mutex::new(State::default()),
        })
    }

    /// The commands to publish now, asking a package again only when it changed.
    ///
    /// # Invariant
    ///
    /// One answer per call: what a client is greeted with and what a call is routed by are
    /// built together, so a name in one is a name in the other.
    pub async fn commands(&self, at: &Publish<'_>) -> Vec<CommandSpec> {
        let mut state = self.state.lock().await;
        let declared = match self.declarations() {
            Ok(declared) => declared,
            Err(why) => {
                let fingerprint = self.fingerprint(&[]);
                if state.fingerprint.as_ref() != Some(&fingerprint) {
                    tracing::warn!(problem = %why, "no declared command could be read");
                    state.fingerprint = Some(fingerprint);
                    state.manifests.clear();
                    state.problems = vec![why];
                }
                return publish(&mut state, at);
            }
        };
        let fingerprint = self.fingerprint(&declared);
        if state.fingerprint.as_ref() != Some(&fingerprint) {
            let (manifests, problems) = self.discover(&declared, at.skills).await;
            for problem in &problems {
                tracing::warn!(problem = %problem, "a declared command was not read");
            }
            state.fingerprint = Some(fingerprint);
            state.manifests = manifests;
            state.problems = problems;
        }
        publish(&mut state, at)
    }

    /// The call a published name runs, or `None` when no declaration answers it.
    ///
    /// # Invariant
    ///
    /// What [`Self::commands`] last published: dispatch looks a name up in the catalog
    /// first, so a name answered here is one that call put in the greeting.
    pub async fn route(&self, name: &str) -> Option<Route> {
        self.state.lock().await.routes.get(name).cloned()
    }

    /// What the content declares, at the commit this process installs.
    fn declarations(&self) -> Result<Vec<crate::ship::Command>, String> {
        let content = crate::ship::content(&self.home).map_err(|e| format!("{e:#}"))?;
        Ok(content.commands().to_vec())
    }

    fn fingerprint(&self, declared: &[crate::ship::Command]) -> Fingerprint {
        let roots = crate::tools::skill::roots(None);
        Fingerprint {
            commit: crate::ship::resolved_commit().unwrap_or_default(),
            declarations: declared
                .iter()
                .map(|c| (c.name.clone(), c.skill.clone(), c.function.clone()))
                .collect(),
            packages: declared
                .iter()
                .filter_map(|c| roots.find(&c.skill))
                .map(|skill| PackageFile::of(skill.dir()))
                .collect(),
        }
    }

    /// Asks every declared package what its functions take; a failure is kept, not fatal.
    async fn discover(
        &self,
        declared: &[crate::ship::Command],
        skills: Option<&Skills>,
    ) -> (Vec<Manifest>, Vec<String>) {
        let mut manifests = Vec::new();
        let mut problems = Vec::new();
        if declared.is_empty() {
            return (manifests, problems);
        }
        let Some(skills) = skills else {
            problems.push(
                "this build was given no skill programs, so no declared command can be read"
                    .to_string(),
            );
            return (manifests, problems);
        };
        let roots = crate::tools::skill::roots(None);
        for declaration in declared {
            match self.manifest(skills, &roots, declaration).await {
                Ok(manifest) => manifests.push(manifest),
                Err(why) => problems.push(format!(
                    "{} ({} of {}): {why}",
                    declaration.name, declaration.function, declaration.skill
                )),
            }
        }
        (manifests, problems)
    }

    /// Asks one package what it publishes, and reads this declaration's function out of it.
    async fn manifest(
        &self,
        skills: &Skills,
        roots: &skill::Roots,
        declaration: &crate::ship::Command,
    ) -> Result<Manifest, String> {
        let Some(skill) = roots.find(&declaration.skill) else {
            let known: Vec<String> = roots.all().iter().map(|skill| skill.name.clone()).collect();
            return Err(format!(
                "no skill is {:?} here; `ls_skill` lists {}",
                declaration.skill,
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
        };
        if !::skill::is_package(skill.dir()) {
            return Err(format!(
                "{} is not a Python package: it has no __init__.py, so it publishes no functions",
                skill.dir().display()
            ));
        }
        let reply = ask(skills, &self.home, skill.dir()).await?;
        Ok(Manifest {
            name: declaration.name.clone(),
            skill: declaration.skill.clone(),
            function: declaration.function.clone(),
            arguments: signature_of(&reply, &declaration.function)?,
        })
    }
}

/// What one package answered its manifest request with.
async fn ask(
    skills: &Skills,
    home: &std::path::Path,
    directory: &std::path::Path,
) -> Result<Value, String> {
    let package = ::skill::package_of(skills.python.as_deref(), skills.lib.as_deref())?;
    let request = format!(
        "{}\n",
        serde_json::json!({
            "skill": directory.display().to_string(),
            "manifest": true,
        })
    );
    let mut env: Vec<(String, String)> = Vec::new();
    if let Some(lib) = skills.lib.as_deref() {
        env.push((
            "GONGGA_SKILL_LIB".to_string(),
            lib.to_string_lossy().into_owned(),
        ));
    }
    let contract = package.contract.display().to_string();
    let outcome = crate::tools::words::run(
        &package.python,
        &[contract],
        Some(&request),
        home,
        &env,
        MANIFEST_DEADLINE,
    )
    .await?;
    if outcome.code != 0 {
        return Err(format!(
            "{} answered exit {}: {}",
            directory.display(),
            outcome.code,
            crate::script::head(&outcome.stderr)
        ));
    }
    reply_of(&outcome.stdout)
}

/// What one package publishes, as one schema document.
///
/// # Invariant
///
/// Read off the file the publisher generated, and trusted as written: every reader of
/// one skill reads that document, so asking the package again here would be a second
/// opinion about the same API, disagreeing with the model's copy the moment a
/// signature moved. See `skill::schema_of`.
pub(crate) fn package_schema(directory: &Path) -> Result<Value, String> {
    let document = ::skill::schema_of(directory)?;
    let functions = document
        .get("functions")
        .and_then(Value::as_object)
        .ok_or("the generated schema has no `functions` object")?;
    let mut names: Vec<Value> = Vec::new();
    let mut arguments = serde_json::Map::new();
    for (function, arguments_of) in functions {
        names.push(Value::String(function.clone()));
        arguments.insert(function.clone(), arguments_of.clone());
    }
    // SPEC's `Cmd := {name, args}`: the names this skill answers to, and the
    // arguments the chosen one takes.
    let mut properties = serde_json::Map::new();
    properties.insert(
        "name".into(),
        serde_json::json!({ "type": "string", "enum": names }),
    );
    let mut chosen = serde_json::Map::new();
    chosen.insert("schema_of".into(), Value::String("name".into()));
    chosen.insert("schemas".into(), Value::Object(arguments));
    let mut args = serde_json::Map::new();
    args.insert(protocol::argschema::EXTENSION.into(), Value::Object(chosen));
    properties.insert("args".into(), Value::Object(args));
    Ok(protocol::argschema::object(properties))
}

/// The arguments every declared command's function takes, by the command's name.
///
/// # Invariant
///
/// The function is the one truth: it names its arguments, kinds them and says which are
/// required, so a tool row of the same name may restate none of that. A kind the function
/// did not state stays empty, and a row is allowed to supply one there. A declaration that
/// cannot be read is left out, said once, so a home whose content predates a package still
/// starts.
pub fn declared(home: &Path) -> std::collections::BTreeMap<String, Vec<Row>> {
    let mut table = std::collections::BTreeMap::new();
    let content = match crate::ship::content(home) {
        Ok(content) => content,
        Err(why) => {
            tracing::warn!(problem = %why, "no declared function's arguments could be read");
            return table;
        }
    };
    let roots = crate::tools::skill::roots(None);
    for command in content.commands() {
        let Some(skill) = roots.find(&command.skill) else {
            tracing::warn!(
                command = %command.name,
                "no skill is {:?} here, so its function's arguments are not read",
                command.skill
            );
            continue;
        };
        match arguments_of(skill.dir(), &command.function) {
            Ok(arguments) => {
                table.insert(command.name.clone(), arguments);
            }
            Err(why) => tracing::warn!(
                command = %command.name,
                function = %command.function,
                "this function's arguments were not read: {why}"
            ),
        }
    }
    table
}

/// One function's arguments, as the schema its package generated states them.
fn arguments_of(directory: &Path, function: &str) -> Result<Vec<Row>, String> {
    let document = ::skill::schema_of(directory)?;
    let Some(schema) = document
        .get("functions")
        .and_then(Value::as_object)
        .and_then(|functions| functions.get(function))
    else {
        return Err(format!(
            "the generated schema of {} publishes no {function:?}",
            directory.display()
        ));
    };
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    Ok(properties
        .iter()
        .map(|(name, property)| Row {
            name: name.clone(),
            // Empty is the function stating no kind, which only a row may fill in.
            kind: property
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            items: property
                .get("items")
                .and_then(|items| items.get("type"))
                .and_then(Value::as_str)
                .map(str::to_string),
            required: required.contains(name),
            positional: None,
            hint: None,
            completion: Default::default(),
            sentence: None,
        })
        .collect())
}

impl PackageFile {
    fn of(dir: &Path) -> Self {
        let path = dir.join("__init__.py");
        let meta = std::fs::metadata(&path).ok();
        Self {
            len: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta.and_then(|m| m.modified().ok()),
            path,
        }
    }
}

/// What the declarations that answered become, and the call each published name runs.
fn publish(state: &mut State, at: &Publish<'_>) -> Vec<CommandSpec> {
    let reserved: HashSet<&str> = at.reserved.iter().map(String::as_str).collect();
    let mut routes = HashMap::new();
    let mut published = Vec::new();
    for manifest in &state.manifests {
        if reserved.contains(manifest.name.as_str()) {
            continue;
        }
        let held = at.tools.get(&manifest.name);
        let row = held.as_ref().map(|tool| tool.spec());
        published.push(spec_of(manifest, row));
        routes.insert(
            manifest.name.clone(),
            Route {
                skill: manifest.skill.clone(),
                function: manifest.function.clone(),
            },
        );
    }
    state.routes = routes;
    published
}

/// The row's argument of that name, when the row declares one.
fn argued<'a>(row: &'a ToolSpec, name: &str) -> Option<&'a Row> {
    row.parameters.iter().find(|argument| argument.name == name)
}

/// One declaration as the catalog entry an operator reads.
///
/// # Invariant
///
/// The argument names, and which of them a call must carry, are the function's own --
/// the contract's answer, never this file's opinion. A row of the same name adds only
/// what prose can say: its sentences, a value's shape, its place on a line.
fn spec_of(manifest: &Manifest, row: Option<&ToolSpec>) -> CommandSpec {
    let description = row.and_then(|row| row.summary.clone()).unwrap_or_else(|| {
        format!(
            "Run `{}` of the `{}` skill.",
            manifest.function, manifest.skill
        )
    });
    let args = manifest
        .arguments
        .iter()
        .map(|argument| {
            let said = row.and_then(|row| argued(row, &argument.name));
            ArgSpec {
                type_of: argument
                    .type_of
                    .clone()
                    .or_else(|| said.map(|said| said.kind.clone()))
                    .unwrap_or_else(|| "str".to_string()),
                name: argument.name.clone(),
                description: said
                    .and_then(|said| said.sentence.clone())
                    .unwrap_or_default(),
                required: argument.required,
                positional: said.and_then(|said| said.positional).is_some(),
                value_hint: said.and_then(|said| said.hint.clone()),
                completions: said
                    .map(|said| said.completion.source())
                    .unwrap_or(CompletionSource::None),
                kind: None,
                accepts: None,
                sticky_to: None,
                schema: None,
                update: false,
            }
        })
        .collect();
    CommandSpec {
        target: CommandTarget::Server,
        name: manifest.name.clone(),
        description,
        args,
    }
}

/// What one function takes, read out of the manifest a package answered with.
fn signature_of(reply: &Value, function: &str) -> Result<Vec<Signature>, String> {
    let functions = reply
        .get("functions")
        .and_then(Value::as_object)
        .ok_or("the manifest has no `functions` object")?;
    let Some(rows) = functions.get(function) else {
        let known: Vec<&String> = functions.keys().collect();
        return Err(format!(
            "the package publishes no {:?}; it publishes {}",
            function,
            if known.is_empty() {
                "nothing".to_string()
            } else {
                known
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<&str>>()
                    .join(", ")
            }
        ));
    };
    let rows = rows
        .as_array()
        .ok_or("a function's arguments are a list of {name, required}")?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let name = row.get("name").and_then(Value::as_str)?;
            Some(Signature {
                name: name.to_string(),
                required: row
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                type_of: row.get("type").and_then(Value::as_str).map(str::to_string),
            })
        })
        .collect())
}

/// The one JSON object a manifest request answered: its last non-empty stdout line.
fn reply_of(stdout: &str) -> Result<Value, String> {
    let Some(line) = stdout.lines().rev().find(|line| !line.trim().is_empty()) else {
        return Err("the manifest request answered nothing on stdout".to_string());
    };
    serde_json::from_str(line).map_err(|e| {
        format!(
            "the manifest is not one JSON object: {e}; it answered {}",
            line.chars().take(200).collect::<String>()
        )
    })
}
