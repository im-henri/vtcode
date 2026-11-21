use super::*;

fn library() -> Retriever {
    let mut retriever = Retriever::default();
    retriever.add(
        1,
        "fn connect_socket(peer_address: Address) { open_channel(peer_address); }",
    );
    retriever.add(
        2,
        "fn render_widget(frame: &mut Frame, widget_area: Rect) { draw_border(frame); }",
    );
    retriever.add(
        3,
        "fn parse_header(raw_header: &str) -> Header { split_fields(raw_header) }",
    );
    // Filler files that all use common names, so those names carry no weight.
    for key in 10..30 {
        retriever.add(
            key,
            &format!("fn helper_{key}(value: u32) -> u32 {{ let result = value; result }}"),
        );
    }
    retriever
}

#[test]
fn identifiers_are_distinct_hashes_of_words_without_numbers_or_short_names() {
    let ids = identifiers("let ab = open_channel(open_channel, x1, 12345, αβγδ);");
    // `let`, `open_channel` and `αβγδ` count; `ab`, `x1` and the number do not.
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert_eq!(identifiers("open_channel open_channel").len(), 1);
    assert!(identifiers("").is_empty() && identifiers("a b 1 22").is_empty());
}

#[test]
fn files_sharing_distinctive_names_rank_first() {
    let retriever = library();
    let found = retriever.query("let peer = peer_address; open_channel(", 0, 3);
    assert_eq!(found.first(), Some(&1), "{found:?}");
    let found = retriever.query("draw_border(frame, widget_area)", 0, 2);
    assert_eq!(found.first(), Some(&2), "{found:?}");
    assert!(
        retriever
            .query("totally unrelated words here", 0, 3)
            .is_empty()
    );
}

#[test]
fn the_active_file_and_common_names_are_ignored() {
    let retriever = library();
    // Only file 1 shares the distinctive name, and it is the active file.
    assert!(
        retriever
            .query("open_channel(peer_address)", 1, 3)
            .is_empty()
    );
    // `value` and `result` appear in twenty files: they rank nothing.
    assert!(retriever.query("let result = value;", 0, 3).is_empty());
}

#[test]
fn limits_and_empty_libraries_are_handled() {
    assert!(Retriever::default().query("open_channel", 0, 3).is_empty());
    let retriever = library();
    assert_eq!(
        retriever
            .query("open_channel peer_address split_fields draw_border", 0, 2)
            .len(),
        2
    );
    assert!(retriever.query("open_channel", 0, 0).is_empty());
    assert_eq!(retriever.len(), 23);
    let mut empty = Retriever::default();
    empty.add(1, "1 2 3");
    assert_eq!(empty.len(), 0, "a file without identifiers is not indexed");
}
