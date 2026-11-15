# 2026-11-15 completion writes the separator, and stops at its own token

User, direct: "client cmd 的补全手感还是很差，比如在前面补全会把后面的东西吞掉，以及没有自动补全逗号。（我们的 json 支持尾逗号）". Two complaints: completing with the caret in the middle of the object swallowed what followed, and no comma is written between members.

## Why it swallowed

`json_value::place_token` measured the token's end by *scanning forward from the
cursor*: a bare key or value ran to the next `,`/`}`/`:` or the end of the text
(`bare_end(text, cursor)`), and a closed string ran to `quoted_end(..).unwrap_or(text.len())`.
`catalog::object_completion` turns `Spot::start`/`Spot::end` into `Offered.start`/`end`,
and `tui::input::apply_completion` does `replace_range(start..end, insert)`. A caret
in the middle of `{"id":"a","model_config":{}}` therefore deleted the comma, the
siblings and the closers — measured, pre-fix, as `/set_session{"id":""id":"abc",e,nd"}`.

The rule now: `start` is where the token begins (its opening quote, or the token's
first character) and `end` is where it *ends* — the caret inside a string being
written, the closing quote of a name already written, the delimiter for a bare
token, the cursor where nothing is written. A completion never reaches past the
token it stands in, and text after the caret is left alone. This is one clause of
`Spot` ("a place, never a parse"), not a new idea: the walker already knew where
the token was, it just did not stop there.

## The comma

A candidate that completes a member whose object can hold another writes `,` —
after the value's closers and *after the caret*, so the comma lands past the
insertion point and a following sibling the operator typed is untouched. Whether
the object can hold another is the schema's answer, not the text's: the text says
nothing until the operator types the closer, and at the moment a member is
completed that closer is not there. `remaining(object, written, filling)` is true
when a field other than the one being filled is not yet stated.

So `/set_session{` + Tab is `{"id":"",` (two fields: `id` and `mode`), while
`/del_session{` + Tab is `{"path":""}` — `del_session` publishes one argument, and
a member nothing can follow is not given a separator.

An empty token (`start == end`) is a *place*, not a refusal: a caret right after
`{` is a name place with nothing typed, and a caret right after `:` is a value
place with nothing typed. `object_completion` used to decline these and the popup
was empty exactly where the operator wanted the next name.

## The JSON the line may carry

`json_text::parse_lenient` reads the object as written, and if that fails tries it
again with the trailing commas dropped outside strings; it keeps the result only
if it parses. One forgiving read for the whole `/name{...}` form, so "our json
supports a trailing comma" is a fact about the code and not only about the model's
output repair (`server/src/llm.rs::repair_arguments` is the same idea, for another
producer). `json_command::parse_object` keeps the strict parser's sentence for a
line nothing can read: a refusal has to name what is wrong.

## Measured

`python scripts/tui_live.py --scenario completion-through-client`, an absolute
`--home`: `send_message{"message":""}` and `del_session{"path":"main"}` reached the
screen, and the submit of `send_message{"message":"hi"}` was accepted. The L3 probe
needs an absolute home because of an unrelated server bug: `fork::common_dir`
(`crates/server/src/fork.rs`) joins a *relative* `--git-common-dir` answer onto the
git directory, so a home named relatively stops being "a clone this server owns" on
the second start. That is a pre-existing defect in this checkout, untouched here;
it needs its own change and the user's go-ahead.