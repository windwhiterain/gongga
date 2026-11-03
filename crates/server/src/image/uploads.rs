//! The provider file a copy was uploaded as, and when that file stops being usable.
//!
//! See `docs/notes/server-pictures.md`.
//!
//! # Invariant
//!
//! A row never records a provider's file id. It records the copy (`pictures.id`), which
//! is this home's own and never expires, and the id is resolved from `uploads` when a
//! request is written. A provider deletes its files -- by expiry, by quota, by another
//! key on the same account -- and an id baked into a transcript would be a transcript
//! that can never be sent again. What is stored stays true; what the provider answers is
//! looked up.

use rusqlite::{OptionalExtension, params};

use crate::store::Store;

/// How long an upload is asked to live: DeepSeek's maximum is 30 days.
///
/// Stated rather than omitted. Omitting `expires_after` makes the file permanent, which
/// sounds better and is worse: permanent files accumulate against the account's file
/// count and storage, and a file that never expires can only ever be deleted by hand.
pub const EXPIRES_AFTER_SECS: i64 = 7 * 24 * 60 * 60;

/// How much life one has to have left to be reused, so a file cannot expire mid-request.
pub const REFRESH_MARGIN_SECS: i64 = 60 * 60;

/// Which provider namespace an upload belongs to.
///
/// # Invariant
///
/// DeepSeek shares files across every API key on one account, so the account is the
/// namespace -- but two homes may point at two endpoints, and a file id from one is not
/// a file id at the other. The scope is the endpoint, never the key: hashing the key in
/// would make one account's already-paid uploads invisible to its other keys.
pub fn scope(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_lowercase()
}

/// A file the provider is holding for `copy`, with the id a request names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub file_id: String,
    /// When the provider stops serving it, in millis since the epoch.
    pub expires_at: i64,
}

impl Upload {
    /// Whether this file still has enough life to answer a request being built now.
    ///
    /// # Invariant
    ///
    /// The margin is why this is asked at all: a file with one second left is still
    /// "valid" and still useless, because the request it names is a stream that takes
    /// minutes. A margin makes the answer safe to act on.
    pub fn reusable_at(&self, now: i64) -> bool {
        self.expires_at - now > REFRESH_MARGIN_SECS * 1000
    }
}

/// The upload this home already made for `copy` at `scope`, valid or not.
///
/// Returned even when stale: the caller needs the id either way -- to reuse it, or to
/// forget it before asking for a new one.
pub fn of(store: &Store, scope: &str, copy: i64) -> anyhow::Result<Option<Upload>> {
    let conn = store.picture_conn();
    let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
    let found = guard
        .query_row(
            "SELECT file_id, expires_at FROM uploads WHERE scope = ?1 AND copy = ?2",
            params![scope, copy],
            |row| {
                Ok(Upload {
                    file_id: row.get(0)?,
                    expires_at: row.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(found)
}

/// Records the file the provider minted for `copy`, replacing whatever was there.
///
/// # Invariant
///
/// One row per `(scope, copy)`: an upload is the current answer for that copy, not a
/// history of answers. A stale id that was not replaced first would come back on the
/// next request, which is why the caller forgets before it re-uploads.
pub fn remember(
    store: &Store,
    scope: &str,
    copy: i64,
    file_id: &str,
    expires_at: i64,
) -> anyhow::Result<()> {
    let conn = store.picture_conn();
    let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
    guard.execute(
        "INSERT INTO uploads (scope, copy, file_id, expires_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(scope, copy) DO UPDATE SET
             file_id = excluded.file_id,
             expires_at = excluded.expires_at",
        params![scope, copy, file_id, expires_at],
    )?;
    Ok(())
}

/// Forgets the file an id named: the provider has refused it, so it is not an answer.
pub fn forget(store: &Store, file_id: &str) -> anyhow::Result<usize> {
    let conn = store.picture_conn();
    let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
    Ok(guard.execute("DELETE FROM uploads WHERE file_id = ?1", params![file_id])?)
}

/// Forgets every file this endpoint was recorded as holding.
///
/// # Invariant
///
/// The hammer for a refusal that did not name a file: the request is rebuilt and every
/// picture is uploaded again. It costs one round of uploads, and the alternative is a
/// session whose pictures are all dead and nothing that says which.
pub fn forget_scope(store: &Store, scope: &str) -> anyhow::Result<usize> {
    let conn = store.picture_conn();
    let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
    Ok(guard.execute("DELETE FROM uploads WHERE scope = ?1", params![scope])?)
}

/// The ids this endpoint knows, oldest expiry first: what a quota failure gives up.
///
/// # Invariant
///
/// Oldest-expiring first, because that is the file whose loss costs the least: it was
/// about to be unusable anyway. `limit` is the caller's batch, not a policy this module
/// knows anything about.
pub fn oldest(store: &Store, scope: &str, limit: usize) -> anyhow::Result<Vec<String>> {
    let conn = store.picture_conn();
    let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
    let mut stmt = guard.prepare(
        "SELECT file_id FROM uploads WHERE scope = ?1 ORDER BY expires_at, copy LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![scope, limit as i64], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// When the expiry lands when the provider did not say: counted from now.
pub fn expiry_assumed(now_ms: i64) -> i64 {
    now_ms + EXPIRES_AFTER_SECS * 1000
}
