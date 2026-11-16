//! Probe: what the catalog's completer offers on a half-typed JSON line, and what
//! the line reads as once the first option is accepted.
//!
//! Temporary. Deleted when the completion rework lands.

use gongga_client::{Catalog, Completer, Completion, token_bounds};
use protocol::{Accepts, ArgSpec, CommandSpec, CompletionSource, StructField, StructSchema};

fn leaf(name: &str, type_of: &str, accepts: Option<Accepts>) -> StructField {
    StructField {
        name: name.to_string(),
        description: format!("the {name} field"),
        object: None,
        source: CompletionSource::None,
        accepts,
        value_hint: Some(name.to_string()),
        model_ref: false,
        type_of: Some(type_of.to_string()),
        kind: None,
        default: None,
    }
}

fn arg(name: &str, type_of: &str, accepts: Option<Accepts>) -> ArgSpec {
    ArgSpec {
        type_of: type_of.to_string(),
        name: name.to_string(),
        description: format!("the {name} argument"),
        required: false,
        positional: false,
        value_hint: Some(name.to_string()),
        kind: None,
        completions: CompletionSource::None,
        accepts,
        sticky_to: None,
        structure: None,
        update: false,
    }
}

fn catalog() -> Catalog {
    let config = StructSchema {
        fields: vec![
            leaf(
                "ctx_limit",
                "str",
                Some(Accepts::Same(vec!["70%".to_string(), "50%".to_string()])),
            ),
            leaf("keep_alive", "bool", None),
        ],
    };
    let session = StructSchema {
        fields: vec![
            leaf("model", "str", None),
            leaf(
                "ctx_limit",
                "str",
                Some(Accepts::Same(vec!["70%".to_string(), "50%".to_string()])),
            ),
            StructField {
                name: "config".to_string(),
                description: "the config field".to_string(),
                object: Some(Box::new(config)),
                ..leaf("config", "SessionConfig", None)
            },
        ],
    };
    let command = CommandSpec {
        name: "set_session".to_string(),
        description: "change a session".to_string(),
        args: vec![
            arg("id", "str", None),
            arg("keep_alive", "bool", None),
            arg("config", "SessionConfig", None).with_structure(session),
        ],
        target: Default::default(),
    };
    let catalog = Catalog::new();
    catalog.set_commands(vec![command]);
    catalog
}

fn show(catalog: &Catalog, line: &str, cursor: usize) {
    let offered = catalog.complete(line, cursor);
    let (start, end, _) = token_bounds(line, cursor);
    println!("--- {line:?} at {cursor}  subject {:?}", offered.subject);
    if offered.items.is_empty() {
        println!("    (nothing)");
    }
    let completion = Completion {
        line: line.to_string(),
        cursor,
        start: offered.start.unwrap_or(start),
        end: offered.end.unwrap_or(end),
        subject: offered.subject.clone(),
        items: offered.items.clone(),
        selected: 0,
    };
    for (index, item) in offered.items.iter().enumerate().take(4) {
        let applied = completion.apply(item).map(|(start, end, text, caret)| {
            let mut whole = String::new();
            whole.push_str(&line[..start]);
            let at = whole.len() + caret - start;
            whole.push_str(&text);
            whole.push_str(&line[end..]);
            (whole, at)
        });
        println!(
            "    [{index}] label {:?} text {:?} snippet {} => {applied:?}",
            item.label, item.new_text, item.snippet
        );
    }
}

fn main() {
    let catalog = catalog();
    let cases: &[&str] = &[
        "/set_session{",
        "/set_session{\"",
        "/set_session{\"i",
        "/set_session{\"id\":",
        "/set_session{\"id\":\"\",",
        "/set_session{\"id\":\"a\",",
        "/set_session{\"keep_alive\":",
        "/set_session{\"keep_alive\":t",
        "/set_session{\"config\":{",
        "/set_session{\"config\":{\"ctx_limit\":\"",
        "/set_session{\"config\":{\"ctx_limit\":\"7",
        "/set_session{\"id\":\"a\"}",
        "/set_session",
    ];
    for line in cases {
        show(&catalog, line, line.len());
        if line.len() > 0 && line.is_char_boundary(line.len() - 1) {
            show(&catalog, line, line.len() - 1);
        }
    }
}
