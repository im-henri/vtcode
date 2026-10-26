//! Immutable active-document snapshots shared by extension invocations.
//! Reads copy only the requested bytes, directly from shared Rope leaves into
//! guest memory. Even a full read needs no retained contiguous host-side copy.
use ropey::Rope;

#[derive(Clone)]
pub(crate) struct Buffer(Rope);

impl Buffer {
    pub(crate) fn from_rope(rope: &Rope) -> Self {
        Self(rope.clone())
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len_bytes()
    }

    /// The caller validates the complete destination before writing. Offsets
    /// are raw bytes and may split a UTF-8 scalar or straddle Rope leaves.
    pub(crate) fn copy_range_into(&self, start: usize, mut destination: &mut [u8]) {
        assert!(start <= self.len() && destination.len() <= self.len() - start);
        if destination.is_empty() {
            return;
        }
        let (chunks, chunk_start, _, _) = self.0.chunks_at_byte(start);
        let mut skip = start - chunk_start;
        for chunk in chunks {
            let bytes = &chunk.as_bytes()[skip..];
            let count = bytes.len().min(destination.len());
            destination[..count].copy_from_slice(&bytes[..count]);
            destination = &mut destination[count..];
            if destination.is_empty() {
                break;
            }
            skip = 0;
        }
        debug_assert!(destination.is_empty());
    }

    // Explicit full copies are useful for test assertions only. Production has
    // no API that materializes or retains document-sized byte buffers.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut bytes = vec![0; self.len()];
        self.copy_range_into(0, &mut bytes);
        bytes
    }

    #[cfg(test)]
    pub(crate) fn shares(&self, other: &Self) -> bool {
        self.0.is_instance(&other.0)
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer").field("len", &self.len()).finish()
    }
}

#[cfg(test)]
#[path = "extension_buffer_tests.rs"]
mod tests;
