//! Resolve VS Code Themes gallery links to their Marketplace VSIX packages.
use std::{io::Read, path::Path, time::Duration};

const LINK_HELP: &str = "Use a theme link such as https://vscodethemes.com/e/publisher.extension/theme-name, or a local theme file.";

pub(crate) fn package_url(source: &str) -> Result<String, String> {
    let url = url::Url::parse(source).map_err(|_| LINK_HELP.to_string())?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("vscodethemes.com" | "www.vscodethemes.com")
        )
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(LINK_HELP.into());
    }
    let mut segments = url
        .path_segments()
        .ok_or(LINK_HELP)?
        .filter(|part| !part.is_empty());
    if segments.next() != Some("e") {
        return Err(LINK_HELP.into());
    }
    let (publisher, extension) = segments
        .next()
        .and_then(|id| id.split_once('.'))
        .ok_or(LINK_HELP)?;
    let valid_id = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if !valid_id(publisher) || !valid_id(extension) {
        return Err(LINK_HELP.into());
    }
    // The remaining slug and preview query (e.g. ?language=py) select a website
    // preview. A VSIX contains every color variant in the identified extension.
    Ok(format!(
        "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/{publisher}/vsextensions/{extension}/latest/vspackage"
    ))
}

pub(crate) fn download(url: &str, path: &Path) -> Result<(), String> {
    let agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("VTCode/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    download_with_agent(&agent, url, path, crate::theme_install::MAX_PACKAGE_SIZE)
}

fn download_with_agent(
    agent: &ureq::Agent,
    url: &str,
    path: &Path,
    limit: u64,
) -> Result<(), String> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| format!("Could not download theme from Marketplace: {error}"))?;
    if response.status() != ureq::http::StatusCode::OK {
        return Err(format!(
            "Could not download theme: unexpected HTTP {}",
            response.status()
        ));
    }
    if response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > limit)
    {
        return Err("Theme package exceeds 100 MiB".into());
    }
    let mut output = std::fs::File::create(path).map_err(|error| error.to_string())?;
    // ureq decodes HTTP gzip responses; bound the actual VSIX bytes even when
    // Content-Length is absent or describes a compressed/chunked response.
    let copied = std::io::copy(
        &mut response.body_mut().as_reader().take(limit + 1),
        &mut output,
    )
    .map_err(|error| format!("Could not download theme from Marketplace: {error}"))?;
    if copied > limit {
        return Err("Theme package exceeds 100 MiB".into());
    }
    if copied == 0 {
        return Err("Marketplace returned an empty theme package".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener};

    #[test]
    fn resolves_gallery_links_and_ignores_preview_language() {
        for link in [
            "https://vscodethemes.com/e/akamud.vscode-theme-onedark/atom-one-dark?language=py",
            "https://www.vscodethemes.com/e/akamud.vscode-theme-onedark/atom-one-dark?language=rust#preview",
            "HTTPS://VSCODETHEMES.COM/e/akamud.vscode-theme-onedark",
        ] {
            assert_eq!(
                package_url(link).unwrap(),
                "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/akamud/vsextensions/vscode-theme-onedark/latest/vspackage"
            );
        }
    }

    #[test]
    fn rejects_invalid_gallery_hosts_and_extension_identifiers() {
        for link in [
            "https://vscodethemes.com.evil.example/e/a.b/dark",
            "https://vscodethemes.com@evil.example/e/a.b/dark",
            "https://user:pass@vscodethemes.com/e/a.b/dark",
            "https://vscodethemes.com:123/e/a.b/dark",
            "http://vscodethemes.com/e/a.b/dark",
            "https://vscodethemes.com/e/a.%2fother/dark",
            "https://vscodethemes.com/e/a.b.c/dark",
            "https://vscodethemes.com/e/.b/dark",
            "https://vscodethemes.com/e/a./dark",
            "https://vscodethemes.com/e/no-id/dark",
            "https://vscodethemes.com/",
        ] {
            assert!(package_url(link).is_err(), "{link}");
        }
    }

    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes =
            format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
        bytes.extend(body);
        bytes
    }

    fn download_fixture(response: Vec<u8>, path: &Path, limit: u64) -> Result<(), String> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let _ = stream.write_all(&response);
        });
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let result =
            download_with_agent(&agent, &format!("http://{address}/theme.vsix"), path, limit);
        server.join().unwrap();
        result
    }

    #[test]
    fn downloads_plain_and_gzipped_responses() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("theme.vsix");
        download_fixture(
            response("200 OK", "Content-Length: 12\r\n", b"vsix payload"),
            &path,
            12,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"vsix payload");
        // gzip-compressed "vsix payload", as returned by Marketplace/CDN servers.
        let gzip = [
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 43, 43, 206, 172, 80, 40, 72, 172, 204, 201, 79, 76,
            1, 0, 195, 79, 221, 151, 12, 0, 0, 0,
        ];
        download_fixture(
            response(
                "200 OK",
                "Content-Encoding: gzip\r\nContent-Length: 32\r\n",
                &gzip,
            ),
            &path,
            64,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"vsix payload");
        assert!(
            download_fixture(
                response("200 OK", "Content-Encoding: gzip\r\n", &gzip),
                &path,
                8
            )
            .unwrap_err()
            .contains("100 MiB")
        );
    }

    #[test]
    fn rejects_http_errors_empty_truncated_and_oversized_downloads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("theme.vsix");
        for bytes in [
            response("404 Not Found", "Content-Length: 0\r\n", b""),
            response("200 OK", "Content-Length: 0\r\n", b""),
            response("200 OK", "Content-Length: 12\r\n", b"short"),
            response("200 OK", "Content-Length: 1000\r\n", b""),
            response("200 OK", "", b"more than sixteen bytes"),
        ] {
            assert!(download_fixture(bytes, &path, 16).is_err());
        }
    }
}
