//! Turning a copy into the content part a request carries.
//!
//! See `docs/notes/server-pictures.md`.
//!
//! # Invariant
//!
//! One answer per copy per request: `Files` memoises what it resolved, so a transcript
//! naming the same picture ten times uploads nothing twice and names one file ten times.
//! And an id it hands out is one the provider still serves -- a stale row is refilled
//! before it is used, never after it fails.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::image::{ImageRef, Uploaded, uploads};

/// What one request knows about the files behind its pictures.
#[derive(Clone)]
pub struct Files {
    store: crate::store::Store,
    uploader: std::sync::Arc<crate::llm::Uploader>,
    scope: String,
    /// Resolved this request: a copy is asked about once, however often it is named.
    ///
    /// # Invariant
    ///
    /// Shared rather than copied, so the clone a pending future carries adds nothing
    /// to what the next row asks and everything to what it need not ask twice.
    resolved: std::sync::Arc<std::sync::Mutex<HashMap<i64, Option<String>>>>,
}

impl Files {
    pub fn new(store: crate::store::Store, uploader: std::sync::Arc<crate::llm::Uploader>) -> Self {
        Self {
            scope: uploader.scope(),
            store,
            uploader,
            resolved: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// The content part for one picture, uploading when the provider holds nothing
    /// usable for the copy.
    ///
    /// # Invariant
    ///
    /// A picture whose copy is still here is never dropped from a request: upload that
    /// fails, or a model that takes no files, degrades to the bytes inline. A transcript
    /// that sent a picture yesterday must send that picture today.
    pub async fn part(&mut self, image: &ImageRef) -> Value {
        let Some(copy) = image.copy else {
            // A row from before copies were recorded, or a picture whose copy never
            // landed: the path is all there is.
            return inline_or_note(image, crate::image::pictures::read_missing(image));
        };
        let known = self
            .resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&copy)
            .cloned();
        if let Some(known) = known {
            let known = &known;
            return match known {
                Some(id) => json!({"type": "input_image", "file_id": id}),
                None => self.inline(image, copy),
            };
        }
        match self.mint(copy).await {
            Some(id) => {
                self.note(copy, Some(id.clone()));
                json!({"type": "input_image", "file_id": id})
            }
            None => {
                self.note(copy, None);
                self.inline(image, copy)
            }
        }
    }

    /// Records what this request decided about one copy.
    fn note(&self, copy: i64, answer: Option<String>) {
        self.resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(copy, answer);
    }

    /// The provider's file for `copy`, reused when it has life left and minted when it
    /// does not.
    async fn mint(&self, copy: i64) -> Option<String> {
        let now = crate::store::now_millis();
        if let Ok(Some(upload)) = uploads::of(&self.store, &self.scope, copy)
            && upload.reusable_at(now)
        {
            return Some(upload.file_id);
        }
        // Not reusable: it expired, or there was never one. The old row is
        // replaced, so a stale id cannot come back.
        let bytes = crate::image::pictures::read(copy)?;
        let media_type = self.media_type(copy)?;
        let uploaded = match self.upload(copy, &media_type, &bytes).await {
            Some(uploaded) => uploaded,
            None => return None,
        };
        if let Err(e) = uploads::remember(
            &self.store,
            &self.scope,
            copy,
            &uploaded.id,
            uploaded.expires_at,
        ) {
            tracing::warn!(copy, error = %e, "the file the provider minted cannot be recorded");
        }
        Some(uploaded.id)
    }

    /// One upload, with the account's quota given a chance to make room.
    ///
    /// # Invariant
    ///
    /// A quota refusal deletes this endpoint's oldest-expiring files and tries once
    /// more. Once, not in a loop: a second refusal is an account that is actually full,
    /// and deleting more of the user's pictures to find that out is not this function's
    /// business.
    async fn upload(&self, copy: i64, media_type: &str, bytes: &[u8]) -> Option<Uploaded> {
        let name = format!("copy-{copy}");
        match self.uploader.upload(&name, media_type, bytes).await {
            Ok(uploaded) => Some(uploaded),
            Err(e) if is_quota(&e.to_string()) => {
                let freed = self.reclaim(100).await;
                tracing::warn!(
                    copy,
                    freed,
                    "the account had no room for one more file; its oldest were deleted"
                );
                self.uploader.upload(&name, media_type, bytes).await.ok()
            }
            Err(e) => {
                tracing::warn!(copy, error = %e, "no file id was minted; the bytes go inline");
                None
            }
        }
    }

    /// Deletes up to `limit` of this endpoint's files, oldest-expiring first, and
    /// forgets each one it deleted.
    async fn reclaim(&self, limit: usize) -> usize {
        let Ok(candidates) = uploads::oldest(&self.store, &self.scope, limit) else {
            return 0;
        };
        let mut freed = 0;
        for file_id in candidates {
            if self.uploader.delete(&file_id).await.is_ok() {
                let _ = uploads::forget(&self.store, &file_id);
                freed += 1;
            }
        }
        freed
    }

    /// The bytes, inline: what a request carries when no file can be named.
    fn inline(&self, image: &ImageRef, copy: i64) -> Value {
        match crate::image::pictures::read(copy) {
            Some(bytes) => inline_bytes(image, &bytes),
            None => note(image),
        }
    }

    /// The media type a copy was stored with, for a file the provider is about to hold.
    fn media_type(&self, copy: i64) -> Option<String> {
        self.store.picture_media_type(copy)
    }
}

/// Whether a provider's refusal is about room, not about the file.
fn is_quota(detail: &str) -> bool {
    let quota =
        detail.contains("quota") || detail.contains("storage") || detail.contains("too many files");
    quota && (detail.contains("429") || detail.contains("400") || detail.contains("403"))
}

fn inline_bytes(image: &ImageRef, bytes: &[u8]) -> Value {
    json!({
        "type": "input_image",
        "image_url": format!(
            "data:{};base64,{}",
            image.media_type,
            crate::base64::encode(bytes)
        ),
    })
}

fn note(image: &ImageRef) -> Value {
    json!({
        "type": "input_text",
        "text": format!(
            "[the image at {} is no longer readable and this home holds no copy, so \
             whatever it showed cannot be recovered]",
            image.path
        ),
    })
}

/// The part for a row that recorded no copy: its path, or a note saying it is gone.
fn inline_or_note(image: &ImageRef, bytes: Option<Vec<u8>>) -> Value {
    match bytes {
        Some(bytes) => inline_bytes(image, &bytes),
        None => note(image),
    }
}
