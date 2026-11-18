//! The page this host serves: a handful of files, carried in the program.
//!
//! See `docs/notes/webui-page.md`.

/// One file the browser asks for, and the type it is read as.
pub struct Asset {
    pub kind: &'static str,
    pub body: &'static str,
}

/// The file a URL names, or `None` when the page does not carry it.
///
/// # Invariant
///
/// The page is embedded, not read from disk: a front-end that shipped beside a
/// checkout instead of a home would serve whatever happened to be in the tree.
pub fn asset(path: &str) -> Option<Asset> {
    let (kind, body) = match path {
        "/" | "/index.html" => (
            "text/html; charset=utf-8",
            include_str!("../web/index.html"),
        ),
        "/style.css" => ("text/css; charset=utf-8", include_str!("../web/style.css")),
        "/main.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/main.js"),
        ),
        "/model.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/model.js"),
        ),
        "/blocks.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/blocks.js"),
        ),
        "/view.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/view.js"),
        ),
        "/cursor.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/cursor.js"),
        ),
        "/intents.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/intents.js"),
        ),
        _ => return None,
    };
    Some(Asset { kind, body })
}
