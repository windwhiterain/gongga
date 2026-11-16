//! Probe: what the sticky entries fill into a structured argument, and what a
//! typed line writes back.
//!
//! Temporary. Deleted when the fill path is settled.

use std::path::Path;

use gongga_client::window_state::Sticky;
use protocol::argschema::{Row, object, property};
use protocol::{ArgSpec, Args, CommandSpec, CompletionSource, NONE_VALUE};

fn leaf(
    description: &str,
    spelling: &str,
    kind: Option<&str>,
    model_ref: bool,
) -> serde_json::Value {
    property(&Row {
        type_of: Some(spelling),
        description,
        object: None,
        accepts: None,
        source: &CompletionSource::None,
        kind,
        hint: None,
        default: None,
        model_ref,
        update: false,
    })
}

fn config_document() -> serde_json::Value {
    let mut model_config = serde_json::Map::new();
    model_config.insert(
        "model".into(),
        leaf(
            "The model this session runs on.",
            "Model",
            Some("model"),
            true,
        ),
    );
    model_config.insert(
        "ctx_limit".into(),
        property(&Row {
            type_of: Some("str"),
            description: "The context level for a Context notice.",
            object: None,
            accepts: None,
            source: &CompletionSource::Static(vec!["70%".to_string(), "50%".to_string()]),
            kind: None,
            hint: Some("n%"),
            default: Some("70%"),
            model_ref: false,
            update: false,
        }),
    );
    let mut properties = serde_json::Map::new();
    properties.insert(
        "model_config".into(),
        property(&Row {
            type_of: Some("ModelConfig"),
            description: "The model and how it is asked.",
            object: Some(object(model_config)),
            accepts: None,
            source: &CompletionSource::None,
            kind: Some("model_config"),
            hint: None,
            default: None,
            model_ref: false,
            update: false,
        }),
    );
    properties.insert(
        "keep_alive".into(),
        leaf(
            "Whether a periodic notice keeps the prefix warm.",
            "bool",
            None,
            false,
        ),
    );
    object(properties)
}

fn spec() -> CommandSpec {
    CommandSpec {
        name: "set_session".to_string(),
        description: "change a session".to_string(),
        args: vec![
            ArgSpec {
                type_of: "Session".to_string(),
                name: "id".to_string(),
                description: "Session to create or reconfigure.".to_string(),
                required: true,
                positional: false,
                value_hint: Some("session".to_string()),
                kind: Some("session".to_string()),
                completions: CompletionSource::Sessions,
                accepts: None,
                sticky_to: None,
                schema: None,
                update: false,
            },
            ArgSpec {
                type_of: "SessionConfig".to_string(),
                name: "config".to_string(),
                description: "The session's settings.".to_string(),
                required: false,
                positional: false,
                value_hint: None,
                kind: Some("config".to_string()),
                completions: CompletionSource::None,
                accepts: None,
                sticky_to: None,
                schema: Some(config_document()),
                update: false,
            },
        ],
        target: Default::default(),
    }
}

fn main() {
    let home = Path::new(".dev/probe-home");
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::remove_file(home.join("client.toml"));
    let mut sticky = Sticky::load(home);
    let _ = sticky.set("session", "\"main\"");
    let _ = sticky.set(
        "model_config",
        "{\"model\":{\"provider\":\"fake\",\"model\":\"stream-model\"}}",
    );
    let _ = sticky.set("model", "\"other/flat-model\"");

    let command = spec();
    let mut args = Args::new();
    sticky.fill(Some(&command), &mut args);
    println!("filled args: {args:?}");

    let mut typed = Args::new();
    typed.insert(
        "id".to_string(),
        serde_json::Value::String("main".to_string()),
    );
    typed.insert(
        "config".to_string(),
        serde_json::json!({
            "model_config": {"model": {"provider": "typed", "model": "from-line"}, "ctx_limit": "50%"},
            "keep_alive": true,
            "unknown_field": 1
        }),
    );
    println!("absorbed: {:?}", sticky.absorb(&command, &typed));
    println!(
        "kinds: {:?}",
        gongga_client::commands::sticky_kinds(&[command])
    );
    println!("none value is {NONE_VALUE:?}");
}
