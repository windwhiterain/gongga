//! Expands `[[model]]` / `[[route]]` / `[[serving]]` into `[[provider]]` rows.
//!
//! See `docs/notes/server-providers.md`.

use std::collections::HashMap;

use toml::{Table, Value};

#[cfg(test)]
mod tests;

/// Expands `[[model]]` / `[[route]]` / `[[serving]]` into `[[provider]]` rows.
///
/// # Invariant
///
/// The three tables are removed as they are read, so the value that comes out is
/// the one shape [`super::parse_value`] knows. A serving naming a route or a model
/// that no table declares is an error naming it: a typo must not become a provider
/// that sells nothing.
pub(super) fn expand(value: &mut Value) -> Result<(), String> {
    let facts = facts(value)?;
    let routes = tables(value, "route");
    let servings = tables(value, "serving");
    if routes.is_empty() && servings.is_empty() {
        return Ok(());
    }
    let mut rows = Vec::with_capacity(routes.len());
    for route in routes {
        let id = route
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            return Err("a [[route]] states no id".to_string());
        }
        if route.contains_key("model") || route.contains_key("models") {
            return Err(format!(
                "route {id:?} declares models; a [[serving]] names which it sells"
            ));
        }
        // Two routes claiming one id are as ambiguous as two providers were, and
        // the first claim wins for the same reason.
        if rows.iter().any(|row: &Table| named(row) == id) {
            continue;
        }
        rows.push(route);
    }
    for serving in servings {
        let route_id = text(&serving, "route")?;
        let model_id = text(&serving, "model")?;
        let facts = facts.get(&model_id).ok_or_else(|| {
            format!("[[serving]] names model {model_id:?}, which no [[model]] declares")
        })?;
        let row = rows
            .iter_mut()
            .find(|row| named(row) == route_id)
            .ok_or_else(|| {
                format!("[[serving]] names route {route_id:?}, which no [[route]] declares")
            })?;
        let served = served(facts, &serving)?;
        let id = named(&served);
        let sold: &mut Vec<Value> = row
            .entry("model".to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| format!("route {route_id:?} states a `model` that is not a list"))?;
        if sold
            .iter()
            .filter_map(Value::as_table)
            .any(|known| named(known) == id)
        {
            return Err(format!(
                "route {route_id:?} sells {id:?} twice; one serving per model per route"
            ));
        }
        sold.push(Value::Table(served));
    }
    let providers = value
        .as_table_mut()
        .ok_or_else(|| "the roster is not a table".to_string())?
        .entry("provider".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| "`provider` is not an array of tables".to_string())?;
    providers.extend(rows.into_iter().map(Value::Table));
    Ok(())
}

/// One model's facts, by the id a serving names it with. First claim wins.
fn facts(value: &mut Value) -> Result<HashMap<String, Table>, String> {
    let mut out = HashMap::new();
    for model in tables(value, "model") {
        let id = named(&model);
        if id.is_empty() {
            return Err("a [[model]] states no id".to_string());
        }
        out.entry(id).or_insert(model);
    }
    Ok(out)
}

/// One served row: the shared facts, then whatever the serving itself states.
///
/// # Invariant
///
/// A serving overrides any field of the model row by writing it -- price and caps
/// differ by route, the rest does not -- except `route` and `model`, which say
/// where the row applies rather than what it says. `wire_id` is the name the
/// route's API takes in its `model` field; without one the shared id is sent.
fn served(facts: &Table, serving: &Table) -> Result<Table, String> {
    let mut row = facts.clone();
    for (key, value) in serving {
        match key.as_str() {
            "route" | "model" => {}
            "wire_id" | "id" => {
                row.insert("id".to_string(), value.clone());
            }
            _ => {
                row.insert(key.clone(), value.clone());
            }
        }
    }
    if named(&row).is_empty() {
        return Err("a [[model]] states no id".to_string());
    }
    // `max_output` is the roster's spelling; this crate reads `max_output_tokens`.
    if !row.contains_key("max_output_tokens")
        && let Some(longest) = row.get("max_output").cloned()
    {
        row.insert("max_output_tokens".to_string(), longest);
    }
    Ok(row)
}

/// The tables under `key`, removed from `value`.
fn tables(value: &mut Value, key: &str) -> Vec<Table> {
    let Some(entry) = value.as_table_mut().and_then(|table| table.remove(key)) else {
        return Vec::new();
    };
    match entry {
        Value::Array(rows) => rows
            .into_iter()
            .filter_map(|row| match row {
                Value::Table(table) => Some(table),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A row's id, or the empty string when it states none.
fn named(row: &Table) -> String {
    row.get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A field the row requires, by name.
fn text(row: &Table, key: &str) -> Result<String, String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("a [[serving]] states no `{key}`"))
}
