use super::expand;

/// The value one fixture expands to, or the refusal it earns.
fn expanded(text: &str) -> Result<toml::Value, String> {
    let mut value: toml::Value = toml::from_str(text).expect("the fixture parses");
    expand(&mut value)?;
    Ok(value)
}

/// The provider row a route expanded into.
fn row<'a>(value: &'a toml::Value, id: &str) -> &'a toml::value::Table {
    value["provider"]
        .as_array()
        .expect("provider rows")
        .iter()
        .filter_map(toml::Value::as_table)
        .find(|row| row["id"].as_str() == Some(id))
        .expect("the route is a provider row")
}

/// The models a route sells.
fn sold<'a>(value: &'a toml::Value, id: &str) -> &'a Vec<toml::Value> {
    row(value, id)["model"].as_array().expect("model rows")
}

const TWO_ROUTES: &str = r#"
[[model]]
id = "ernie"
name = "ERNIE"
context = 1000000
max_output = 65536

[[route]]
id = "plan"
key_env = "PLAN_KEY"

[[route]]
id = "metered"
key_env = "METERED_KEY"

[[serving]]
route = "plan"
model = "ernie"

[[serving]]
route = "metered"
model = "ernie"
wire_id = "ERNIE-WIRE"
price = { input = 2.0, output = 8.0 }
"#;

#[test]
fn one_model_sold_on_two_routes_is_one_set_of_facts() {
    let value = expanded(TWO_ROUTES).expect("two routes over one model expand");
    assert!(value.get("model").is_none(), "the facts are read, not left");
    assert!(value.get("serving").is_none(), "the servings are read");

    let plan = sold(&value, "plan");
    let metered = sold(&value, "metered");
    assert_eq!(plan.len(), 1);
    assert_eq!(metered.len(), 1);
    assert_eq!(plan[0]["context"].as_integer(), Some(1000000));
    assert_eq!(metered[0]["context"].as_integer(), Some(1000000));
    assert_eq!(plan[0]["max_output_tokens"].as_integer(), Some(65536));
    assert!(
        plan[0].get("price").is_none(),
        "the plan states no per-token price"
    );
    assert_eq!(metered[0]["price"]["input"].as_float(), Some(2.0));
}

#[test]
fn a_wire_id_is_the_name_that_route_takes() {
    let value = expanded(TWO_ROUTES).expect("two routes over one model expand");
    assert_eq!(sold(&value, "plan")[0]["id"].as_str(), Some("ernie"));
    assert_eq!(
        sold(&value, "metered")[0]["id"].as_str(),
        Some("ERNIE-WIRE")
    );
}

#[test]
fn a_serving_of_a_model_nobody_declared_is_refused() {
    let text = TWO_ROUTES.replace("model = \"ernie\"\nwire_id", "model = \"gemini\"\nwire_id");
    let refused = expanded(&text).expect_err("an unknown model is refused");
    assert!(
        refused.contains("gemini"),
        "the refusal names it: {refused}"
    );
}

#[test]
fn a_route_declaring_its_own_models_is_refused() {
    let text = TWO_ROUTES.replace(
        "id = \"plan\"\nkey_env = \"PLAN_KEY\"",
        "id = \"plan\"\nkey_env = \"PLAN_KEY\"\n[[route.model]]\nid = \"ernie\"\nname = \"ERNIE\"",
    );
    let refused = expanded(&text).expect_err("an inline model is refused");
    assert!(
        refused.contains("serving"),
        "the refusal says where: {refused}"
    );
}
