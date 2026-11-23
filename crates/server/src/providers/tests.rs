use super::{Model, Price};

/// A row with nothing stated but its id, the way a `[[serving]]` starts.
fn bare(id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        context: None,
        unmasked_context: None,
        endpoints: Vec::new(),
        max_output_tokens: None,
        reasoning_levels: Vec::new(),
        reasoning_default: None,
        reasoning_mapping: Default::default(),
        modalities: Vec::new(),
        builtin_tools: Vec::new(),
        price: None,
        tier: None,
        defaults: Default::default(),
    }
}

/// What a relay's own catalogue says about one model it sells.
fn discovered(id: &str) -> Model {
    Model {
        name: format!("{id} (relay)"),
        context: Some(262_144),
        endpoints: vec!["openai".to_string()],
        max_output_tokens: Some(32_768),
        reasoning_levels: vec!["low".to_string(), "high".to_string()],
        reasoning_default: Some("high".to_string()),
        modalities: vec!["text".to_string()],
        price: Some(Price {
            input: Some(0.3),
            cache_read: Some(0.03),
            output: Some(1.2),
        }),
        tier: Some("pro".to_string()),
        ..bare(id)
    }
}

#[test]
fn a_stated_fact_survives_what_a_route_claims() {
    let mut shared = bare("qwen3.8-max");
    shared.name = "Qwen 3.8 Max".to_string();
    shared.context = Some(1_000_000);
    shared.modalities = vec!["text".to_string(), "image".to_string()];

    assert!(shared.fill_gaps_from(&discovered("qwen3.8-max")));

    // The roster's words, not the relay's: it sells the same model, not a smaller one.
    assert_eq!(shared.name, "Qwen 3.8 Max");
    assert_eq!(shared.context, Some(1_000_000));
    assert_eq!(shared.modalities, vec!["text", "image"]);
    // What only the relay knew arrives, and it is the reason the row is shared at all.
    assert_eq!(shared.price.unwrap().output, Some(1.2));
    assert_eq!(shared.max_output_tokens, Some(32_768));
    assert_eq!(shared.reasoning_levels, vec!["low", "high"]);
}

#[test]
fn a_row_that_states_nothing_takes_everything() {
    let mut empty = bare("glm-5.3");
    assert!(empty.fill_gaps_from(&discovered("glm-5.3")));
    assert_eq!(empty.name, "glm-5.3 (relay)");
    assert_eq!(empty.context, Some(262_144));
    assert_eq!(empty.tier.as_deref(), Some("pro"));
}

#[test]
fn a_row_with_nothing_to_gain_says_so() {
    let settled = discovered("deepseek-v4-pro");
    let mut copy = settled.clone();
    // Everything is stated, so the roster reads the same: no republish.
    assert!(!copy.fill_gaps_from(&settled));
    assert_eq!(copy.context, settled.context);
}
