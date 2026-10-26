use super::*;

#[test]
fn path_codec_preserves_wire_bytes_across_borrowed_owned_escaped_and_streaming_inputs() {
    let pattern: Vec<u8> = (1..=255).filter(|byte| *byte != b'/').collect();
    for length in [
        0, 1, 2, 3, 190, 191, 192, 193, 767, 768, 769, 32766, 32767, 32768,
    ] {
        let bytes = (0..length)
            .map(|index| pattern[index % pattern.len()])
            .collect::<Vec<_>>();
        for absolute in [false, true] {
            let mut path = RemotePath::new(bytes.clone()).unwrap();
            path.1 = absolute;
            let encoded = format!(
                "{}{}",
                if absolute { "/" } else { "" },
                URL_SAFE_NO_PAD.encode(&bytes)
            );
            let expected = serde_json::to_vec(&encoded).unwrap();
            assert_eq!(
                serde_json::to_vec(&path).unwrap(),
                expected,
                "length={length}, absolute={absolute}"
            );
            assert_eq!(
                serde_json::from_slice::<RemotePath>(&expected).unwrap(),
                path
            );
            assert_eq!(
                serde_json::from_reader::<_, RemotePath>(std::io::Cursor::new(&expected)).unwrap(),
                path
            );
            assert_eq!(
                serde_json::from_value::<RemotePath>(serde_json::Value::String(encoded.clone()))
                    .unwrap(),
                path
            );
            // Escaped JSON uses the deserializer's scratch buffer rather than
            // borrowing the original slice. Its path identity must still match.
            let escaped = encoded
                .chars()
                .map(|character| format!("\\u{:04x}", character as u32))
                .collect::<String>();
            assert_eq!(
                serde_json::from_str::<RemotePath>(&format!("\"{escaped}\"")).unwrap(),
                path
            );
            let map = std::collections::HashMap::from([(path.clone(), 7)]);
            let json = serde_json::to_vec(&map).unwrap();
            assert_eq!(
                serde_json::from_slice::<std::collections::HashMap<RemotePath, u8>>(&json).unwrap()
                    [&path],
                7
            );
        }
    }
}

#[test]
fn path_codec_rejects_invalid_or_oversized_decoded_paths() {
    for bytes in [
        b"nul\0".as_slice(),
        b"../outside",
        b"a//b",
        b"a/./b",
        b"/absolute",
        &vec![b'a'; RemotePath::MAX_BYTES + 1],
    ] {
        for prefix in ["", "/"] {
            let encoded = format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes));
            let json = serde_json::to_vec(&encoded).unwrap();
            assert!(serde_json::from_slice::<RemotePath>(&json).is_err());
        }
    }
    for encoded in ["?", "YQ==", "//", "💾"] {
        let json = serde_json::to_vec(encoded).unwrap();
        assert!(serde_json::from_slice::<RemotePath>(&json).is_err());
    }
    for invalid in ["null", "42", "[]", "{}"] {
        assert!(serde_json::from_str::<RemotePath>(invalid).is_err());
    }
}

#[test]
#[ignore = "manual directory/status path-codec comparison against the previous allocating implementation"]
fn benchmark_remote_path_codec() {
    use std::{hint::black_box, time::Instant};
    struct Allocated<'a>(&'a RemotePath);
    impl Serialize for Allocated<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let path = self.0;
            let mut encoded = String::with_capacity(
                base64::encoded_len(path.0.len(), false).unwrap() + usize::from(path.1),
            );
            if path.1 {
                encoded.push('/');
            }
            URL_SAFE_NO_PAD.encode_string(&path.0, &mut encoded);
            serializer.serialize_str(&encoded)
        }
    }
    struct AllocatedDecode(RemotePath);
    impl<'de> Deserialize<'de> for AllocatedDecode {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let value = String::deserialize(deserializer)?;
            let bytes = URL_SAFE_NO_PAD
                .decode(value.strip_prefix('/').unwrap_or(&value))
                .map_err(serde::de::Error::custom)?;
            let mut path = RemotePath::new(bytes).map_err(serde::de::Error::custom)?;
            path.1 = value.starts_with('/');
            Ok(Self(path))
        }
    }
    let paths = (0..100_000)
        .map(|index| {
            let mut bytes =
                format!("src/module_{}/sub/long component-λ {index}.rs", index % 64).into_bytes();
            if index % 17 == 0 {
                bytes.push(255);
            }
            let mut path = RemotePath::new(bytes).unwrap();
            path.1 = index % 10 == 0;
            path
        })
        .collect::<Vec<_>>();
    let old = paths.iter().map(Allocated).collect::<Vec<_>>();
    let expected = serde_json::to_vec(&old).unwrap();
    assert_eq!(serde_json::to_vec(&paths).unwrap(), expected);
    assert_eq!(
        serde_json::from_slice::<Vec<RemotePath>>(&expected).unwrap(),
        paths
    );
    let decoded = serde_json::from_slice::<Vec<AllocatedDecode>>(&expected).unwrap();
    assert!(decoded.iter().zip(&paths).all(|(old, path)| old.0 == *path));
    let mut output = Vec::with_capacity(expected.len());
    let mut elapsed = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for pass in 0..10 {
        // Alternate order to avoid consistently favouring a warmer endpoint.
        for offset in 0..4 {
            let mode = (pass + offset) % 4;
            let started = Instant::now();
            match mode {
                0 => {
                    output.clear();
                    serde_json::to_writer(&mut output, black_box(&old)).unwrap();
                    black_box(&output);
                }
                1 => {
                    output.clear();
                    serde_json::to_writer(&mut output, black_box(&paths)).unwrap();
                    black_box(&output);
                }
                2 => {
                    black_box(
                        serde_json::from_slice::<Vec<AllocatedDecode>>(black_box(&expected))
                            .unwrap(),
                    );
                }
                _ => {
                    black_box(
                        serde_json::from_slice::<Vec<RemotePath>>(black_box(&expected)).unwrap(),
                    );
                }
            }
            if pass != 0 {
                elapsed[mode].push(started.elapsed().as_secs_f64() * 1000.);
            }
        }
    }
    let medians = elapsed.map(|mut samples| {
        samples.sort_by(f64::total_cmp);
        samples[samples.len() / 2]
    });
    eprintln!(
        "path_codec paths={} json_bytes={} encode_before_ms={:.3} encode_after_ms={:.3} decode_before_ms={:.3} decode_after_ms={:.3}",
        paths.len(),
        expected.len(),
        medians[0],
        medians[1],
        medians[2],
        medians[3]
    );
}
