use super::*;

fn complete(before: &str, prefix: &str, after: &str, explicit: bool) -> CompletionResult {
    let text = format!("{before}{prefix}{after}");
    let active = VtCompleteSnapshot {
        source: Default::default(),
        rope: ropey::Rope::from_str(&text),
        document_id: 41,
        cursor_line: before.matches('\n').count(),
        cursor_col: before
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count()
            + prefix.chars().count(),
        active: true,
    };
    compute_contextual_invoked(
        &active,
        prefix,
        "Unknown language",
        &Default::default(),
        &mut TokenCache::default(),
        explicit,
        || false,
    )
    .unwrap()
}

#[test]
fn numeric_predictions_reach_popup_as_values_including_single_digits() {
    for explicit in [false, true] {
        for (before, prefix, after, expected) in [
            ("wait(1)\nwait(2)\nwait(", "", "", "3"),
            ("values = [10, 20, ", "", "]", "30"),
            ("values = [0.25, 0.50, ", "", "]", "0.75"),
            ("values = [9, 6, ", "", "", "3"),
            ("values = [10, 20, ", "3", "]", "30"),
            ("wait(3)\nwait(2)\nwait(", "", "", "1"),
            ("id = 009\nid = 010\nid = ", "", "", "011"),
            ("mask = 0x09\nmask = 0x0a\nmask = ", "0x", "", "0x0b"),
            ("opacity = 0.10\nopacity = 0.20\nopacity = ", "", "", "0.30"),
            ("set(1, 10)\nset(2, 20)\nset(3, 30)\nset(8, ", "", "", "80"),
            ("wait(1)\nwait(2)\nwait(3)\nwait(", "", "", "4"),
            (
                "timeout = 100\ntimeout = 200\ntimeout = 300\ntimeout = ",
                "4",
                "",
                "400",
            ),
            (
                "mask = 0x10\nmask = 0x20\nmask = 0x30\nmask = ",
                "0x",
                "",
                "0x40",
            ),
            (
                "opacity = 0.25\nopacity = 0.50\nopacity = 0.75\nopacity = ",
                "",
                "",
                "1.00",
            ),
            ("wait(1)\nwait(2)\nwait(3)\nwait(", "", "999)", "4"),
        ] {
            let result = complete(before, prefix, after, explicit);
            assert_eq!(
                result.suggestions.first().map(String::as_str),
                Some(expected),
                "{before}{prefix}{after}"
            );
            assert_eq!(result.details[0].kind, super::super::CompletionKind::Value);
            assert_eq!(result.details[0].annotation, "number sequence");
            assert_eq!(result.suggestions.len(), result.details.len());
        }
    }
}

#[test]
fn weak_numeric_evidence_preserves_normal_completion_and_respects_lexical_domain() {
    let result = complete("wait(1)\nwait(2)\nwait(3)\nreadPacket\nwait(", "", "", true);
    assert!(result.suggestions.iter().any(|word| word == "readPacket"));
    assert!(
        result
            .details
            .iter()
            .all(|detail| !detail.annotation.starts_with("number"))
    );
    for (before, after) in [
        ("wait(1)\nwait(2)\nwait(3)\n// wait(", ""),
        ("wait(1)\nwait(2)\nwait(3)\n\"wait(", ""),
        (
            "wait(1, fixed)\nwait(2, fixed)\nwait(3, fixed)\nwait(",
            ", wrong)",
        ),
        ("wait(1)\nwait(2)\nwait(3)\nother(", ""),
        ("wait(1)\nwait(2)\nwait(4)\nwait(", ""),
        (
            "opacity = 0.10\nopacity = 0.20\nopacity = 0.45\nopacity = ",
            "",
        ),
        ("set(1, 10)\nset(2, 20)\nset(8, ", ""),
        ("values = [10, 20, 45, ", "]"),
        ("values = [10, 20, ", ", 50]"),
        ("draw(10, 20, ", ")"),
        ("obj[10, 20, ", "]"),
    ] {
        let result = complete(before, "", after, true);
        assert!(
            result
                .details
                .iter()
                .all(|detail| !detail.annotation.starts_with("number")),
            "{before}{after}"
        );
    }
}
