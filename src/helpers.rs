/// Clamp a `usize` length to `u16` range, saturating at `u16::MAX`.
/// Use instead of bare `.len() as u16` to prevent silent truncation.
#[inline]
pub(crate) fn len_as_u16(len: usize) -> u16 {
    len.min(u16::MAX as usize) as u16
}

/// All possible terminal row widths share one read-only buffer. UTF-8
/// validation happens at compile time, not on every rendering call.
static SPACE_BUF: &str = match std::str::from_utf8(&[b' '; u16::MAX as usize]) {
    Ok(spaces) => spaces,
    Err(_) => panic!("ASCII spaces are valid UTF-8"),
};

/// Screen-width padding is allocation-free; larger non-UI runs are owned so
/// repeated calls cannot leak memory.
#[inline]
pub(crate) fn spaces(n: usize) -> std::borrow::Cow<'static, str> {
    if n <= SPACE_BUF.len() {
        std::borrow::Cow::Borrowed(&SPACE_BUF[..n])
    } else {
        std::borrow::Cow::Owned(" ".repeat(n))
    }
}

/// 128-char static buffer of full-block characters (█, U+2588, 3 bytes each = 384 bytes).
#[cfg(test)]
const BLOCK_BUF: &str = concat!(
    "████████████████████████████████████████████████████████████████",
    "████████████████████████████████████████████████████████████████",
);

/// Return a `&'static str` of `n` block chars (█), zero-allocation for n ≤ 128.
/// Used by the minimap reference renderer in tests.
#[cfg(test)]
#[inline]
pub(crate) fn blocks(n: usize) -> &'static str {
    let byte_len = n * 3; // █ is 3 bytes in UTF-8
    if byte_len <= BLOCK_BUF.len() {
        &BLOCK_BUF[..byte_len]
    } else {
        Box::leak("█".repeat(n).into_boxed_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u16_zero() {
        assert_eq!(len_as_u16(0), 0);
    }

    #[test]
    fn u16_small_value() {
        assert_eq!(len_as_u16(42), 42);
    }

    #[test]
    fn u16_max() {
        assert_eq!(len_as_u16(65535), 65535);
    }

    #[test]
    fn u16_overflow_saturates() {
        assert_eq!(len_as_u16(65536), 65535);
        assert_eq!(len_as_u16(1_000_000), 65535);
    }

    #[test]
    fn spaces_zero() {
        assert_eq!(spaces(0), "");
    }

    #[test]
    fn spaces_small() {
        assert_eq!(spaces(5), "     ");
        assert_eq!(spaces(5).len(), 5);
    }

    #[test]
    fn spaces_exact_buf() {
        let s = spaces(512);
        assert_eq!(s.len(), 512);
        assert!(s.chars().all(|c| c == ' '));
    }

    #[test]
    fn spaces_all_ascii() {
        assert!(spaces(100).is_ascii());
    }

    #[test]
    fn wide_render_padding_reuses_storage_and_oversize_runs_are_owned() {
        for width in [0, 512, 513, 4_096, u16::MAX as usize, 65_536, 100_000] {
            let first = spaces(width);
            let second = spaces(width);
            assert_eq!(first.len(), width);
            assert!(first.bytes().all(|byte| byte == b' '));
            assert_eq!(first, second);
            assert_eq!(matches!(first, std::borrow::Cow::Borrowed(_)), width <= u16::MAX as usize);
            if width <= u16::MAX as usize {
                assert_eq!(first.as_ptr(), second.as_ptr());
            } else {
                assert_ne!(first.as_ptr(), second.as_ptr());
            }
        }
    }

    #[test]
    fn blocks_zero() {
        assert_eq!(blocks(0), "");
    }

    #[test]
    fn blocks_small() {
        assert_eq!(blocks(3), "███");
        assert_eq!(blocks(3).chars().count(), 3);
    }

    #[test]
    fn blocks_char_count() {
        let b = blocks(10);
        assert_eq!(b.chars().count(), 10);
        assert!(b.chars().all(|c| c == '█'));
    }
}
