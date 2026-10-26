//! Compare Git object identities before opening/decompressing immutable blobs.
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Revisions {
    pub base: Option<[u8; 20]>,
    pub index: Option<[u8; 20]>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Sources {
    pub revisions: Revisions,
    // None means missing, unchanged, or identical to base (for index).
    // Object IDs distinguish these cases without duplicate blob payloads.
    pub base: Option<Arc<str>>,
    pub index: Option<Arc<str>>,
}

impl Sources {
    /// Initial responses contain the bytes. Conditional callers must instead
    /// retain their cached contents when the corresponding object ID matches.
    pub(crate) fn index_content(&self) -> &str {
        self.index
            .as_deref()
            .or_else(|| {
                (self.revisions.index == self.revisions.base)
                    .then_some(self.base.as_deref())
                    .flatten()
            })
            .unwrap_or_default()
    }
}

pub(crate) fn read(
    repo: &git2::Repository,
    relative: &Path,
    staged: bool,
    known: Option<Revisions>,
) -> Result<Sources, String> {
    let index = crate::app_git::index_file_id(repo, relative)?;
    let base = if staged {
        crate::app_git::head_file_id(repo, relative, true)?
    } else {
        index
    };
    let bytes = |id: git2::Oid| <[u8; 20]>::try_from(id.as_bytes()).unwrap();
    let revisions = Revisions {
        base: base.map(bytes),
        index: index.map(bytes),
    };
    let base_changed = known.is_none_or(|known| known.base != revisions.base);
    let index_changed = staged && known.is_none_or(|known| known.index != revisions.index);
    let base_content = if base_changed {
        crate::app_git::blob_file_content(repo, base)?
    } else {
        None
    };
    let index_content = if index_changed {
        if base == index {
            None
        } else {
            crate::app_git::blob_file_content(repo, index)?
        }
    } else {
        None
    };
    Ok(Sources {
        revisions,
        base: base_content,
        index: index_content,
    })
}
