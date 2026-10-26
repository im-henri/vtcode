use super::*;

#[test]
fn packed_counts_round_trip_all_domains_and_u16_boundaries() {
    let mut catalog = Catalog::default();
    let rows = [
        ("codeWord", 1, 0, [1, 0, 0]),
        ("calledWord", u16::MAX, u16::MAX, [u16::MAX, 0, 0]),
        ("stringWord", 7, 0, [0, 7, 0]),
        ("commentWord", 300, 0, [0, 0, 300]),
        ("mixedWord", 42, 13, [20, 12, 10]),
        ("uncategorizedWord", 9, 2, [0, 0, 0]),
    ];
    let source: Vec<_> = rows
        .iter()
        .map(|&(word, frequency, calls, domains)| FileTerm {
            word: word.into(),
            frequency,
            calls,
            domains,
            contexts: FileContexts::default(),
        })
        .collect();
    let record = FileRecord {
        path: "/packed.custom".into(),
        stamp: None,
        hash: 7,
        terms: Arc::new(source),
        examples: Arc::default(),
        dirty: true,
        memory_bytes: 0,
        reducible_bytes: 0,
    };
    let packed = catalog.pack(record, &IndexSettings::default());
    let decoded = packed.decoded(&catalog);
    for &(word, frequency, calls, domains) in &rows {
        let item = decoded
            .terms
            .iter()
            .find(|item| item.word.as_ref() == word)
            .unwrap();
        assert_eq!(
            (item.frequency, item.calls, item.domains),
            (frequency, calls, domains)
        );
    }
    assert!(packed.terms.words.len() < rows.len() * 16);
    catalog.remove_record(&packed);
    assert_eq!(catalog.memory_bytes, 0);
}

#[test]
fn word_pages_release_empty_pages_and_reuse_ids_without_stale_spellings() {
    let mut words = Words::default();
    let mut ids = Vec::new();
    for i in 0..1024 {
        let value: Arc<str> = format!("word{i}").into();
        ids.push(words.insert(Word {
            key: key(&value),
            frequency: 1,
            calls: 0,
            domains: [1, 0, 0],
            owners: 1,
        }));
    }
    let allocated = words.memory();
    for &id in &ids[..768] {
        words.remove(id);
    }
    assert!(words.memory() < allocated / 2);
    let id = words.insert(Word {
        key: key("new"),
        frequency: 1,
        calls: 0,
        domains: [1, 0, 0],
        owners: 1,
    });
    assert_eq!(id, 767);
    assert_eq!(words.get(id).unwrap().spelling(), "new");
    assert_eq!(words.get(1000).unwrap().spelling(), "word1000");
}

#[test]
fn malformed_varints_stop_at_the_internal_codec_boundary() {
    for bytes in [&[255u8; 6][..], &[128][..], &[255, 255, 255, 255, 127][..]] {
        assert!(Reader::new(bytes).get().is_none());
    }
}

#[test]
fn saturated_statistics_cannot_release_an_id_still_owned_by_other_files() {
    let mut catalog = Catalog::default();
    let mut term = [FileTerm {
        word: "sharedWord".into(),
        frequency: u16::MAX,
        calls: 0,
        domains: [u16::MAX, 0, 0],
        contexts: Vec::new(),
    }];
    let records: Vec<_> = (0..70_000).map(|_| catalog.add(&mut term)).collect();
    let id = records[0].terms.iter().next().unwrap().id;
    assert_eq!(catalog.words.get(id).unwrap().frequency, u32::MAX);
    // 65,537 * 65,535 reaches the aggregate's saturated value exactly.
    for record in &records[..65_537] {
        catalog.remove_record(record);
    }
    assert_eq!(catalog.spelling(id), Some("sharedWord"));
    let last = records.last().unwrap();
    let file = crate::completion_source::Source::new(Some(&last.path), "").file;
    assert_eq!(
        catalog.vocabulary(file, "shared"),
        vec![("sharedWord".into(), u32::from(u16::MAX), 0)]
    );
    for record in &records[65_537..] {
        catalog.remove_record(record);
    }
    assert_eq!(catalog.memory_bytes, 0);
}
