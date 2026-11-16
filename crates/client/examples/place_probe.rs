//! Probe: the place read by `json_place` against the one read by `json_value`.
//!
//! Temporary. Deleted when the completion rework lands.

use gongga_client::json_place::{self, Typing};
use gongga_client::json_value;

fn main() {
    let tails: &[&str] = &[
        "",
        "{",
        "{\"",
        "{\"i",
        "{\"id",
        "{\"id\"",
        "{\"id\":",
        "{\"id\":\"",
        "{\"id\":\"a",
        "{\"id\":\"a\"",
        "{\"id\":\"a\",",
        "{\"id\":\"a\",\"config\":{",
        "{\"id\":\"a\",\"config\":{\"model_config\":{",
        "{\"id\":\"a\"}",
        "{\"model\":\"deepseek-official/deepseek-flash\",",
        "{\"keep_alive\":t",
        "{\"keep_alive\":true",
        "{\"keep_alive\":true,",
        "{\"ctx_limit\":\"70%",
        "{\"a\":[1,2],",
        "{\"a\":tru,\"b\":",
        "{\"message\":\"hi\",",
        "{\"a\":{\"b\":\"c\"}}",
        "{\"中文\":\"值",
        "{\"path\":\"C:\\\\resource",
        "{\"model\":{\"provider\":\"deepseek\",\"model\":\"flash\"},",
        "{\"a\":1} tail",
    ];
    let mut checked = 0usize;
    let mut differed = 0usize;
    let mut old_none = 0usize;
    let mut new_none = 0usize;
    let mut gained = 0usize;
    let mut other = 0usize;
    for tail in tails {
        for at in 0..=tail.len() {
            if !tail.is_char_boundary(at) {
                continue;
            }
            checked += 1;
            let old = json_value::spot(tail, at);
            let new = json_place::place(tail, at);
            if old.is_none() {
                old_none += 1;
            }
            if new.is_none() {
                new_none += 1;
            }
            let same = match (&old, &new) {
                (None, None) => true,
                (Some(old), Some(new)) => {
                    old.start == new.start
                        && old.end == new.end
                        && old.path == new.path
                        && matches!(
                            (old.typing, new.typing),
                            (json_value::Typing::Key, Typing::Key)
                                | (json_value::Typing::Value, Typing::Value)
                        )
                        && old.fragment == new.fragment
                        && old.quoted == new.quoted
                        && old.open == new.open
                        && old.after == new.after
                        && format!("{:?}", old.fields) == format!("{:?}", new.fields)
                }
                _ => false,
            };
            if same {
                continue;
            }
            differed += 1;
            match (&old, &new) {
                (Some(old), None) => println!("REGRESS {tail:?} at {at}\n  old {old:?}"),
                (None, Some(_)) => gained += 1,
                (Some(old), Some(new)) => {
                    other += 1;
                    if other <= 25 {
                        println!("--- {tail:?} at {at}\n  old {old:?}\n  new {new:?}");
                    }
                }
                (None, None) => {}
            }
        }
    }
    println!(
        "checked {checked}, differed {differed}, old none {old_none}, new none {new_none}, gained {gained}, other {other}"
    );
    println!("== closed ==");
    for tail in tails {
        println!("{tail:?} -> {:?}", json_place::close(tail).text);
    }
}
