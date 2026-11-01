//! SPEC's `output (colored markdown)`, drawn without losing anything.
//!
//! # Invariant
//!
//! What is drawn is what the model wrote: a marker this window has no rendering
//! for is printed as itself. A reader must be able to reconstruct the output from
//! the screen, so nothing is silently dropped to make a row look tidier.
//! See `docs/notes/tui-markdown.md`.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// The columns one nesting level adds in front of a list item.
const STEP: &str = "  ";

/// `text` as the rows an answer is read in.
///
/// # Invariant
///
/// Block structure becomes row structure -- a paragraph is rows, a list item is a
/// row -- and every source marker is printed: `#`, `-`, `1.`, `>`, ``` fences,
/// `*`/`**`/`~~`, `[text](url)`. The one thing a source break does not decide is
/// where a row ends: the window is narrower than the document was written, so the
/// log wraps. See `docs/notes/tui-markdown.md`.
pub fn render(text: &str) -> String {
    let options = Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut out = String::new();
    let mut line = Start::default();
    // Where each open link's target is, until its `](…)` is due.
    let mut targets: Vec<String> = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote = 0usize;
    let mut code = false;

    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                line.open(quote, &mut out);
                out.push_str(&"#".repeat(level as usize));
                out.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => line.close(&mut out),
            // A paragraph is its rows and carries no marker; ending one ends a row.
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => line.close(&mut out),
            Event::Start(Tag::List(start)) => {
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
                    _ => out.push_str("- "),
                }
            }
            Event::End(TagEnd::Item) => line.close(&mut out),
            Event::Start(Tag::BlockQuote(_)) => {
                line.close(&mut out);
                quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                line.close(&mut out);
                quote = quote.saturating_sub(1);
            }
            // The fence is part of what was written, language included.
            Event::Start(Tag::CodeBlock(kind)) => {
                line.close(&mut out);
                line.open(quote, &mut out);
                out.push_str("```");
                if let CodeBlockKind::Fenced(language) = kind {
                    out.push_str(language.trim());
                }
                line.close(&mut out);
                code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                line.close(&mut out);
                line.open(quote, &mut out);
                out.push_str("```");
                line.close(&mut out);
                code = false;
            }
            Event::Text(text) => {
                if code {
                    // A code block's rows are its own: no mark in front, so the
                    // indentation survives. Its trailing newline is no blank row.
                    for part in text.split('\n') {
                        if part.is_empty() {
                            continue;
                        }
                        line.open(quote, &mut out);
                        out.push_str(part.trim_end());
                        line.close(&mut out);
                    }
                    continue;
                }
                line.open(quote, &mut out);
                out.push_str(&text);
            }
            Event::Code(code) => {
                line.open(quote, &mut out);
                out.push('`');
                out.push_str(code.trim());
                out.push('`');
            }
            // Emphasis is kept as it was written: this window has one style per
            // block, so it has no way to draw `**bold**` as bold.
            Event::Start(Tag::Emphasis) => {
                line.open(quote, &mut out);
                out.push('*');
            }
            Event::End(TagEnd::Emphasis) => {
                line.open(quote, &mut out);
                out.push('*');
            }
            Event::Start(Tag::Strong) => {
                line.open(quote, &mut out);
                out.push_str("**");
            }
            Event::End(TagEnd::Strong) => {
                line.open(quote, &mut out);
                out.push_str("**");
            }
            Event::Start(Tag::Strikethrough) => {
                line.open(quote, &mut out);
                out.push_str("~~");
            }
            Event::End(TagEnd::Strikethrough) => {
                line.open(quote, &mut out);
                out.push_str("~~");
            }
            // A link is its text and its target: the target is information, and
            // this window has no way to follow it instead.
            Event::Start(Tag::Link { dest_url, .. }) => {
                line.open(quote, &mut out);
                out.push('[');
                targets.push(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => {
                line.open(quote, &mut out);
                out.push(']');
                out.push('(');
                out.push_str(&targets.pop().unwrap_or_default());
                out.push(')');
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                line.open(quote, &mut out);
                out.push_str("![");
                targets.push(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                line.open(quote, &mut out);
                out.push(']');
                out.push('(');
                out.push_str(&targets.pop().unwrap_or_default());
                out.push(')');
            }
            Event::SoftBreak => {
                // A soft break is a space in the document, not a row: the row
                // breaks are the log's, at the width it actually has.
                line.open(quote, &mut out);
                out.push(' ');
            }
            Event::HardBreak => line.close(&mut out),
            Event::Rule => {
                line.close(&mut out);
                line.open(quote, &mut out);
                out.push_str("---");
                line.close(&mut out);
            }
            // The item's own `- ` already said this is a list; the box says which
            // kind, and it is what the writer typed.
            Event::TaskListMarker(done) => {
                line.open(quote, &mut out);
                out.push_str(if done { "[x] " } else { "[ ] " });
            }
            // Anything a future parser adds still arrives as `Text`, which is
            // where it is drawn.
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
            out.push_str("> ");
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

    /// Nothing the model wrote is missing from the screen.
    ///
    /// # Invariant
    ///
    /// Walked through the markers one at a time, because each of them was dropped
    /// by an earlier version of this file for being untidy on screen.
    #[test]
    fn every_marker_the_writer_used_is_still_there() {
        for (source, want) in [
            ("**bold**", "**bold**"),
            ("*italic*", "*italic*"),
            ("~~gone~~", "~~gone~~"),
            ("`code`", "`code`"),
            ("[text](https://x.test/a)", "[text](https://x.test/a)"),
            ("![alt](img.png)", "![alt](img.png)"),
            ("> quoted", "> quoted"),
            ("- item", "- item"),
            ("1. first", "1. first"),
            ("# Head", "# Head"),
            ("### Deep", "### Deep"),
            ("---", "---"),
        ] {
            assert_eq!(render(source), want, "{source:?}");
        }
    }

    #[test]
    fn a_code_block_keeps_its_fence_and_language() {
        assert_eq!(
            render("```rust\nfn main() {}\n```"),
            "```rust\nfn main() {}\n```"
        );
        // Indentation inside the block is the document, so it survives.
        assert_eq!(render("```\n  kept\n```"), "```\n  kept\n```");
        assert_eq!(render("```\nno language\n```"), "```\nno language\n```");
    }

    #[test]
    fn a_paragraph_is_one_row_however_the_source_broke_it() {
        // A soft break is a space in the document; the log decides the rows.
        assert_eq!(render("one\ntwo"), "one two");
        assert_eq!(render("one\n\ntwo"), "one\ntwo");
    }

    #[test]
    fn a_nested_list_indents_by_two_and_keeps_its_marks() {
        assert_eq!(render("- a\n  - b"), "- a\n  - b");
        assert_eq!(render("1. a\n2. b"), "1. a\n2. b");
    }

    #[test]
    fn a_quote_marks_every_row_it_covers() {
        assert_eq!(render("> one\n>\n> two"), "> one\n> two");
        assert_eq!(render("> outer\n>\n> > inner"), "> outer\n> > inner");
    }

    #[test]
    fn a_task_list_says_whether_it_is_done() {
        assert_eq!(render("- [x] done\n- [ ] not"), "- [x] done\n- [ ] not");
    }

    #[test]
    fn a_link_inside_a_sentence_keeps_its_target() {
        assert_eq!(
            render("see [the docs](https://x.test/d) for more"),
            "see [the docs](https://x.test/d) for more"
        );
    }

    #[test]
    fn nothing_left_over_ends_in_a_blank_row() {
        for text in ["", "a", "- a\n", "# h\n", "```\nx\n```\n"] {
            assert!(!render(text).ends_with('\n'), "{text:?}");
        }
    }

    /// An unfinished marker is text -- the parser's reading of a document still
    /// being written -- and it stays on screen.
    #[test]
    fn an_unfinished_marker_is_shown_as_itself() {
        assert_eq!(render("**bol"), "**bol");
        assert_eq!(render("```rust\nfn f("), "```rust\nfn f(\n```");
    }
}
