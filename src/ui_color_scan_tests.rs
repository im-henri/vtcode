use super::*;

fn reference_colors(line: &str, check_named: bool) -> Vec<(usize, u8, u8, u8)> {
    let mut results = Vec::new();
    if line.len() > COLOR_DETECT_MAX_LINE_LEN {
        return results; // skip very long lines for performance
    }
    let bytes = line.as_bytes();
    let is_ascii = line.is_ascii();
    // Build byte-offset to char-column mapping for multibyte safety
    // Skip allocation for pure ASCII lines where byte offset == char column
    let byte_to_col_vec: Vec<usize>;
    let byte_to_col: &[usize];
    if is_ascii {
        byte_to_col = &[]; // unused for ASCII; we use index directly
    } else {
        let mut v = Vec::with_capacity(bytes.len() + 1);
        let mut col = 0usize;
        for ch in line.chars() {
            for _ in 0..ch.len_utf8() {
                v.push(col);
            }
            col += 1;
        }
        v.push(col); // sentinel
        byte_to_col_vec = v;
        byte_to_col = &byte_to_col_vec;
    }
    // Helper to get char column from byte index
    let col_at = |i: usize| -> usize { if is_ascii { i } else { byte_to_col[i] } };

    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' {
            // Try #RRGGBBAA (8 hex digits) or #RRGGBB (6 hex digits)
            if (i + 8 < bytes.len()
                && bytes[i + 1..i + 9].iter().all(|b| b.is_ascii_hexdigit())
                && (i + 9 >= bytes.len() || !bytes[i + 9].is_ascii_hexdigit()))
                || (i + 6 < bytes.len()
                    && bytes[i + 1..i + 7].iter().all(|b| b.is_ascii_hexdigit())
                    && (i + 7 >= bytes.len() || !bytes[i + 7].is_ascii_hexdigit()))
            {
                if let (Ok(r), Ok(g), Ok(b_val)) = (
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 3..i + 5]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 5..i + 7]).unwrap_or(""), 16),
                ) {
                    results.push((col_at(i), r, g, b_val));
                }
            }
            // Try #RGB (3 hex digits)
            else if i + 3 < bytes.len()
                && bytes[i + 1..i + 4].iter().all(|b| b.is_ascii_hexdigit())
                && (i + 4 >= bytes.len() || !bytes[i + 4].is_ascii_hexdigit())
            {
                let r = bytes[i + 1];
                let g = bytes[i + 2];
                let b_ch = bytes[i + 3];
                if let (Ok(rv), Ok(gv), Ok(bv)) = (
                    u8::from_str_radix(std::str::from_utf8(&[r]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&[g]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&[b_ch]).unwrap_or(""), 16),
                ) {
                    results.push((col_at(i), rv * 17, gv * 17, bv * 17));
                }
            }
        }
        // Detect rgb(r, g, b) and rgba(r, g, b, a)
        if b == b'r' && i + 4 < bytes.len() {
            let is_rgba = i + 5 < bytes.len() && &bytes[i..i + 5] == b"rgba(";
            let is_rgb = !is_rgba && &bytes[i..i + 4] == b"rgb(";
            if is_rgb || is_rgba {
                let start = if is_rgba { i + 5 } else { i + 4 };
                if let Some(end) = bytes[start..].iter().position(|&c| c == b')') {
                    let inner = std::str::from_utf8(&bytes[start..start + end]).unwrap_or("");
                    // Parse CSV without allocating a Vec
                    let mut parts = inner.splitn(4, ',');
                    if let (Some(p0), Some(p1), Some(p2)) =
                        (parts.next(), parts.next(), parts.next())
                    {
                        if let (Ok(r), Ok(g), Ok(bv)) = (
                            p0.trim().parse::<u8>(),
                            p1.trim().parse::<u8>(),
                            p2.trim().parse::<u8>(),
                        ) {
                            results.push((col_at(i), r, g, bv));
                        }
                    }
                }
            }
        }
        // Detect hsl(h, s%, l%) and hsla(h, s%, l%, a)
        if b == b'h' && i + 4 < bytes.len() {
            let is_hsla = i + 5 < bytes.len() && &bytes[i..i + 5] == b"hsla(";
            let is_hsl = !is_hsla && &bytes[i..i + 4] == b"hsl(";
            if is_hsl || is_hsla {
                let start = if is_hsla { i + 5 } else { i + 4 };
                if let Some(end) = bytes[start..].iter().position(|&c| c == b')') {
                    let inner = std::str::from_utf8(&bytes[start..start + end]).unwrap_or("");
                    // Parse CSV without allocating a Vec
                    let mut parts = inner.splitn(4, ',');
                    if let (Some(p0), Some(p1), Some(p2)) =
                        (parts.next(), parts.next(), parts.next())
                    {
                        let h_str = p0.trim();
                        let s_str = p1.trim().trim_end_matches('%');
                        let l_str = p2.trim().trim_end_matches('%');
                        if let (Ok(h), Ok(s), Ok(l)) = (
                            h_str.parse::<f32>(),
                            s_str.parse::<f32>(),
                            l_str.parse::<f32>(),
                        ) {
                            let (r, g, bv) = hsl_to_rgb(h, s / 100.0, l / 100.0);
                            results.push((col_at(i), r, g, bv));
                        }
                    }
                }
            }
        }
    }
    if check_named {
        // Extract words and compare case-insensitively without allocating
        // a full lowercase copy of the line.
        let mut word_start = None;
        for i in 0..=bytes.len() {
            let is_boundary = i == bytes.len() || is_word_boundary(bytes[i]);
            if is_boundary {
                if let Some(start) = word_start {
                    let word = &line[start..i];
                    // CSS color names are ASCII, so we can lowercase just the word
                    let mut buf = [0u8; 24]; // longest CSS color name is 20 chars
                    if word.len() <= buf.len() && word.is_ascii() {
                        let wbytes = word.as_bytes();
                        buf[..word.len()].copy_from_slice(wbytes);
                        buf[..word.len()].make_ascii_lowercase();
                        if let Ok(lower_word) = std::str::from_utf8(&buf[..word.len()]) {
                            if let Some(&(r, g, b)) = CSS_COLOR_MAP.get(lower_word) {
                                results.push((col_at(start), r, g, b));
                            }
                        }
                    }
                    word_start = None;
                }
            } else if word_start.is_none() {
                word_start = Some(i);
            }
        }
    }
    results
}

#[test]
fn color_candidates_preserve_unicode_columns_names_and_malformed_functions() {
    let atoms = [
        "界 ",
        "é",
        "🌍 ",
        "#aBc ",
        "#01234567;",
        "#123456;",
        "#abcd ",
        "#123456789 ",
        "rgb(1, 2, 3)",
        "rgba(0, 255, 17, .5)",
        "hsl(240,100%,50%)",
        "hsla(120, 75%, 30%, 1)",
        "rgba(",
        "hsl(",
        ") ",
        " RED ",
        "rebeccapurple",
        "darkblue_",
        "notacolor ",
        "\t",
        "rgb(界)",
        "rgb(rgb(1,2,3))",
    ];
    for named in [false, true] {
        for left in atoms {
            for middle in atoms {
                let text = format!("{left}{middle}界 #abc RED rgb(4,5,6) tail");
                assert_eq!(
                    find_inline_colors(&text, named),
                    reference_colors(&text, named),
                    "{text:?}, named={named}"
                );
            }
        }
        for len in [1, 20, 200, 500, 667, 1000, 2000] {
            for repeated in ["界", "rgb(", "#123 ", " RED ", "hsl(0, 0%, 0%) "] {
                let text = repeated.repeat(len);
                assert_eq!(
                    find_inline_colors(&text, named),
                    reference_colors(&text, named)
                );
            }
        }
    }
    assert_eq!(
        find_inline_colors("界 #abc CYAN", true),
        vec![(2, 170, 187, 204), (7, 0, 255, 255)]
    );
}

#[test]
#[ignore = "comparative release benchmark"]
fn benchmark_editor_color_candidates() {
    use std::{hint::black_box, time::Instant};
    for (case, text, named) in [
        (
            "unicode code",
            "    // 界 café implementation details without color values;".repeat(8),
            false,
        ),
        (
            "ascii code",
            "    const value = calculate(input_values); // calculate again".repeat(8),
            false,
        ),
        (
            "css colors",
            "界 { color: rebeccapurple; background: #123abc; border: rgb(1, 2, 3); }".repeat(8),
            true,
        ),
        ("unfinished functions", "rgb(hsl(".repeat(200), false),
    ] {
        assert_eq!(
            find_inline_colors(&text, named),
            reference_colors(&text, named)
        );
        let mut old = Vec::new();
        let mut new = Vec::new();
        for round in 0..9 {
            for optimized in [round % 2 == 0, round % 2 != 0] {
                let start = Instant::now();
                for _ in 0..4_000 {
                    let input = black_box(text.as_str());
                    black_box(if optimized {
                        find_inline_colors(input, named)
                    } else {
                        reference_colors(input, named)
                    });
                }
                let elapsed = start.elapsed() / 4_000;
                if optimized {
                    new.push(elapsed);
                } else {
                    old.push(elapsed);
                }
            }
        }
        old.sort_unstable();
        new.sort_unstable();
        eprintln!(
            "colors {case}: before {:?}, after {:?}, {:.2}x",
            old[4],
            new[4],
            old[4].as_secs_f64() / new[4].as_secs_f64()
        );
    }
}
