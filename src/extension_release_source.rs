//! GitHub release identities. Installation does not invoke Git.
use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtensionSource {
    pub repository: String,
    pub reference: Option<String>,
    pub owner: String,
    pub project: String,
}

impl ExtensionSource {
    pub(crate) fn repository_key(&self) -> String {
        format!("{}/{}", self.owner, self.project).to_ascii_lowercase()
    }

    pub(crate) fn parse(input: &str) -> Result<Self, String> {
        const HELP: &str = "Use a GitHub repository link or a published release link, such as https://github.com/owner/extension/releases/tag/v1.0.0.";
        let input = input.trim();
        if input.chars().any(char::is_control) {
            return Err(HELP.into());
        }
        let normalized = input
            .strip_prefix("git@github.com:")
            .map(|path| format!("https://github.com/{path}"));
        let url = url::Url::parse(normalized.as_deref().unwrap_or(input)).map_err(|_| HELP)?;
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
        {
            return Err(HELP.into());
        }
        let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
        let valid = |value: &str| {
            !value.is_empty()
                && value != "."
                && value != ".."
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        let owner = *parts.first().ok_or(HELP)?;
        let project = parts.get(1).ok_or(HELP)?.trim_end_matches(".git");
        if !valid(owner) || !valid(project) {
            return Err(HELP.into());
        }
        let mut reference = match parts.as_slice() {
            [_, _] | [_, _, "releases"] | [_, _, "tags"] => None,
            [_, _, "releases", "tag", tag] => Some(decode(tag)),
            _ => return Err("Install a published release package, not a branch or source tree. Paste the repository link to choose a release.".into()),
        };
        if let Some(fragment) = url.fragment() {
            for (key, value) in url::form_urlencoded::parse(fragment.replace('+', "%2B").as_bytes())
            {
                if key != "tag" || reference.is_some() {
                    return Err("Use #tag=v1.0.0 to select one release. Branches and package subdirectories are not supported.".into());
                }
                reference = Some(value.into_owned());
            }
        }
        if reference
            .as_ref()
            .is_some_and(|tag| semantic_version(tag).is_none())
        {
            return Err(
                "Release tags must be semantic versions, such as v1.0.0 or v1.1.0-rc.1.".into(),
            );
        }
        Ok(Self {
            repository: format!("https://github.com/{owner}/{project}"),
            reference,
            owner: owner.into(),
            project: project.into(),
        })
    }

    pub(crate) fn api(&self, suffix: &str) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/{suffix}",
            self.owner, self.project
        )
    }
}

fn decode(value: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", value.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

pub(crate) fn semantic_version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("VTCode/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Credentials stay in this short-lived worker client, never in UI state,
/// provenance, URLs, or diagnostics. Cross-host redirects drop authorization.
pub(crate) struct GitHubClient {
    agent: ureq::Agent,
    token: Option<String>,
}

impl GitHubClient {
    pub(crate) fn new() -> Self {
        let token = ["GH_TOKEN", "GITHUB_TOKEN"]
            .into_iter()
            .find_map(|name| {
                std::env::var(name)
                    .ok()
                    .and_then(|token| valid_token(&token))
            })
            .or_else(|| {
                let mut command = Command::new("gh");
                command
                    .args(["auth", "token", "--hostname", "github.com"])
                    .env("GH_PROMPT_DISABLED", "1");
                command_token(command, Duration::from_secs(5))
            });
        Self {
            agent: agent(),
            token,
        }
    }

    pub(crate) fn get(
        &self,
        url: &str,
        accept: &str,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        if !url::Url::parse(url).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("api.github.com")
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
        }) {
            return Err("GitHub credentials can only be sent to the GitHub HTTPS API.".into());
        }
        let mut request = self.agent.get(url).header("Accept", accept);
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request.call().map_err(request_error)
    }
}

fn valid_token(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 4096 && value.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| value.to_owned())
}

fn command_token(mut command: Command, timeout: Duration) -> Option<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut token = String::new();
    child
        .stdout
        .take()?
        .take(4097)
        .read_to_string(&mut token)
        .ok()?;
    valid_token(&token)
}

pub(crate) fn request_error(error: impl std::fmt::Display) -> String {
    format!(
        "GitHub release request failed: {error}. Check the connection, repository link, and account access. For private repositories, run gh auth login or set GH_TOKEN/GITHUB_TOKEN. If GitHub's rate limit was reached, retry later."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_are_bounded_and_cannot_escape_to_other_hosts_or_errors() {
        for token in ["", "header\ninjection", "two tokens", "non-ascii-世界"] {
            assert!(valid_token(token).is_none());
        }
        assert_eq!(
            valid_token(" fake_token\r\n").as_deref(),
            Some("fake_token")
        );
        let client = GitHubClient {
            agent: agent(),
            token: Some("private_test_credential".into()),
        };
        for url in [
            "https://example.com/asset",
            "http://api.github.com/repos/a/b",
            "https://api.github.com.example.org/",
            "https://api.github.com:444/",
            "https://user@api.github.com/",
        ] {
            let error = client.get(url, "application/json").unwrap_err();
            assert!(!error.contains("private_test_credential"));
        }
        assert!(matches!(
            client.agent.config().redirect_auth_headers(),
            ureq::config::RedirectAuthHeaders::SameHost
        ));
    }
    #[test]
    fn credential_command_output_is_captured_without_logging_or_inheriting_stdin() {
        #[cfg(windows)]
        let mut command = Command::new("cmd");
        #[cfg(windows)]
        command.args(["/d", "/c", "echo fake_token"]);
        #[cfg(unix)]
        let mut command = Command::new("sh");
        #[cfg(unix)]
        command.args(["-c", "printf fake_token"]);
        assert_eq!(
            command_token(command, Duration::from_secs(5)).as_deref(),
            Some("fake_token")
        );
    }

    #[test]
    #[ignore = "Authenticated GitHub access to the three private extension repositories"]
    fn github_release_metadata_uses_existing_signin_for_private_repositories() {
        let client = GitHubClient::new();
        for project in [
            "vtcode-ext-hex-editor",
            "vtcode-ext-tabular-workbench",
            "vtcode-ext-waveform-viewer",
        ] {
            let source =
                ExtensionSource::parse(&format!("https://github.com/im-henri/{project}")).unwrap();
            let response = client
                .get(
                    &source.api("releases?per_page=1"),
                    "application/vnd.github+json",
                )
                .unwrap();
            assert_eq!(response.status(), ureq::http::StatusCode::OK);
        }
    }
}
