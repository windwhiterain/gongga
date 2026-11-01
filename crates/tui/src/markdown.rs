//! SPEC's `output (colored markdown)`, structure only.
//!
//! # Invariant
//!
//! What is kept is the shape of the document -- headings, list items, quotes,
//! code -- and what is dropped is the marking: emphasis markers are taken by the
//! parser and never drawn, because this log gives one look to a whole block and a
//! style inside a row is not something it can carry. See `docs/notes/tui-markdown.md`.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// The columns one nesting level adds in front of a list item or a quote.
const STEP: &str = "  ";

/// `text` as the rows an answer is read in.
///
/// # Invariant
///
/// Every line it returns ends with `\n` except the last, so the log's own wrap is
/// the only thing that decides where a row breaks: a soft break inside a paragraph
/// becomes a space, because this window is narrower than the document was written.
pub fn render(text: &str) -> String {
    let mut out = String::new();
    let mut line = Start::default();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote = 0usize;
    let mut code = false;
    let mut first_in_item = false;

    for event in Parser::new_ext(text, Options::ENABLE_TASKLISTS) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                line.open(quote, &mut out);
                out.push_str(&"#".repeat(level as usize));
                out.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => line.close(&mut out),
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => line.close(&mut out),
            Event::Start(Tag::List(start)) => {
                // A list opening mid-line ends it: items never share a row.
                line.close(&mut out);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                if lists.pop().is_some() && lists.is_empty() {
                    line.close(&mut out);
                }
            }
            Event::Start(Tag::Item) => {
                line.close(&mut out);
                line.open(quote, &mut out);
                for _ in 1..lists.len() {
                    out.push_str(STEP);
                }
                match lists.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("\u{2022} "),
                }
                first_in_item = true;
            }
            Event::End(TagEnd::Item) => {
                line.close(&mut out);
                first_in_item = false;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                line.close(&mut out);
                quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                line.close(&mut out);
                quote = quote.saturating_sub(1);
            }
            Event::Start(Tag::CodeBlock(_)) => {
                line.close(&mut out);
                code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                line.close(&mut out);
                code = false;
            }
            Event::Text(text) => {
                // Inside a code block there are no items and no quote marks: the
                // text is the document, so it is drawn as it was written.
                if code {
                    for (i, part) in text.split('\n').enumerate() {
                        if i > 0 {
                            line.close(&mut out);
                        }
                        if !part.trim_end().is_empty() || i == 0 {
                            line.open(quote, &mut out);
                            out.push_str(STEP);
                        }
                        out.push_str(part.trim_end());
                    }
                    continue;
                }
                line.open(quote, &mut out);
                let _ = first_in_item;
                out.push_str(&text);
            }
            Event::Code(code) => {
                // Structure, not decoration: the backticks stay, so a reader can
                // tell a command from prose about one.
                line.open(quote, &mut out);
                out.push('`');
                out.push_str(code.trim());
                out.push('`');
            }
            Event::SoftBreak => {
                line.open(quote, &mut out);
                out.push(' ');
            }
            Event::HardBreak => line.close(&mut out),
            Event::Rule => {
                line.close(&mut out);
                line.open(quote, &mut out);
                out.push_str("\u{2500}\u{2500}\u{2500}");
                line.close(&mut out);
            }
            Event::TaskListMarker(done) => {
                line.open(quote, &mut out);
                out.push_str(if done { "[x] " } else { "[ ] " });
            }
            // Links keep their text and drop their target: a terminal cannot
            // follow a URL, and drawing it would be the longer row.
            Event::Start(Tag::Link { .. }) | Event::End(TagEnd::Link) => {}
            Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => {}
            _ => {}
        }
    }
    line.close(&mut out);
    // The log draws the rows it is given; a trailing newline would be a blank row.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Whether the current row has been opened, and what it still owes.
#[derive(Default)]
struct Start {
    opened: bool,
}

impl Start {
    /// Opens a row with the quote marks it sits inside.
    fn open(&mut self, quote: usize, out: &mut String) {
        if self.opened {
            return;
        }
        for _ in 0..quote {
            out.push_str("\u{2502} ");
        }
        self.opened = true;
    }

    /// Ends the row, if one was open.
    fn close(&mut self, out: &mut String) {
        if !self.opened {
            return;
        }
        out.push('\n');
        self.opened = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_is_one_row_however_the_source_broke_it() {
        assert_eq!(render("one\ntwo"), "one two");
        assert_eq!(render("one\n\ntwo"), "one\ntwo");
    }

    #[test]
    fn a_heading_keeps_its_level() {
        assert_eq!(render("# Title\n\ntext"), "# Title\ntext");
        assert_eq!(render("### Deep"), "### Deep");
    }

    #[test]
    fn a_list_is_bulleted_and_counted() {
        assert_eq!(render("- a\n- b"), "\u{2022} a\n\u{2022} b");
        assert_eq!(render("1. a\n2. b"), "1. a\n2. b");
    }

    #[test]
    fn a_nested_list_indents_by_two() {
        assert_eq!(render("- a\n  - b"), "\u{2022} a\n  \u{2022} b");
    }

    #[test]
    fn a_quote_carries_a_mark_on_every_row() {
        assert_eq!(render("> said"), "\u{2502} said");
    }

    #[test]
    fn a_code_block_is_kept_as_written() {
        assert_eq!(render("```rust\nfn main() {}\n```"), "  fn main() {}");
    }

    #[test]
    fn emphasis_markers_are_taken_and_code_keeps_its_backticks() {
        assert_eq!(render("a **bold** word"), "a bold word");
        assert_eq!(render("run `ls -l` now"), "run `ls -l` now");
    }

    #[test]
    fn a_task_list_says_whether_it_is_done() {
        assert_eq!(
            render("- [x] done\n- [ ] not"),
            "\u{2022} [x] done\n\u{2022} [ ] not"
        );
    }

    #[test]
    fn nothing_left_over_ends_in_a_blank_row() {
        for text in ["", "a", "- a\n", "# h\n"] {
            assert!(!render(text).ends_with('\n'), "{text:?}");
        }
    }
}
