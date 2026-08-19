//! "Login with GitHub/Slack/Linear" — RFC 8252's native-app pattern:
//! authorization code + PKCE, redirected to a short-lived local listener
//! this process spins up for the duration of one login.
//!
//! GitHub's token exchange requires a `client_secret` regardless of PKCE
//! (confirmed from GitHub's own docs — PKCE is supplementary there, not a
//! substitute; GitHub Desktop ships one for exactly this reason). Slack and
//! Linear both support genuinely secret-free PKCE — a Slack app must have
//! PKCE enabled to accept this plain-HTTP loopback redirect at all, which
//! is also why the scope below is requested as `user_scope` rather than
//! `scope`: PKCE-enabled Slack apps can only request user-token scopes.
//!
//! The four constants below are baked in at compile time from environment
//! variables (see `build.rs`) — a workspace-root `.env` file for local
//! development, or GitHub Actions secrets in CI. See README.md's
//! "Connecting GitHub/Slack/Linear" section for exactly what to register
//! with each provider and where those values go.

use std::{fmt, time::Duration};

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Every provider offering "Login with..." uses the exact same flow in
/// [`run_login`] — only the details in [`OAuthProvider::endpoints`]/
/// [`OAuthProvider::client_id`]/[`authorize_url`]/[`extract_token`] differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthProvider {
    GitHub,
    Slack,
    Linear,
}

impl fmt::Display for OAuthProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OAuthProvider::GitHub => "GitHub",
            OAuthProvider::Slack => "Slack",
            OAuthProvider::Linear => "Linear",
        })
    }
}

/// Registered at each provider with this exact callback URL — see
/// README.md. RFC 8252 loopback redirects don't strictly need a fixed
/// port (GitHub in particular ignores the port when matching), but Slack
/// and Linear's docs don't confirm the same tolerance, so one fixed port
/// registered everywhere is the simplest thing that works for all three.
const REDIRECT_PORT: u16 = 53682;

fn redirect_uri() -> String {
    format!("http://127.0.0.1:{REDIRECT_PORT}/callback")
}

// Baked in by build.rs from the environment — see this module's doc
// comment. `run_login` fails fast with a message pointing at README.md if
// left unset (empty string, since build.rs always emits a value).
const GITHUB_CLIENT_ID: &str = env!("GITHUB_CLIENT_ID");
const GITHUB_CLIENT_SECRET: &str = env!("GITHUB_CLIENT_SECRET");
const SLACK_CLIENT_ID: &str = env!("SLACK_CLIENT_ID");
const LINEAR_CLIENT_ID: &str = env!("LINEAR_CLIENT_ID");

struct Endpoints {
    authorize_url: &'static str,
    token_url: &'static str,
}

impl OAuthProvider {
    fn endpoints(self) -> Endpoints {
        match self {
            OAuthProvider::GitHub => Endpoints {
                authorize_url: "https://github.com/login/oauth/authorize",
                token_url: "https://github.com/login/oauth/access_token",
            },
            OAuthProvider::Slack => Endpoints {
                authorize_url: "https://slack.com/oauth/v2/authorize",
                token_url: "https://slack.com/api/oauth.v2.access",
            },
            OAuthProvider::Linear => Endpoints {
                authorize_url: "https://linear.app/oauth/authorize",
                token_url: "https://api.linear.app/oauth/token",
            },
        }
    }

    fn client_id(self) -> &'static str {
        match self {
            OAuthProvider::GitHub => GITHUB_CLIENT_ID,
            OAuthProvider::Slack => SLACK_CLIENT_ID,
            OAuthProvider::Linear => LINEAR_CLIENT_ID,
        }
    }

    fn is_configured(self) -> bool {
        !self.client_id().is_empty()
            && (self != OAuthProvider::GitHub || !GITHUB_CLIENT_SECRET.is_empty())
    }
}

/// Runs one complete login: opens the browser, waits for the redirect,
/// exchanges the code for a token. The token is a plain string on success
/// — the caller saves it into `IntegrationsConfig` exactly like a
/// manually-pasted one.
pub async fn run_login(provider: OAuthProvider) -> Result<String, String> {
    if !provider.is_configured() {
        return Err(format!(
            "{provider} isn't configured yet — see README.md's \"Connecting \
             GitHub/Slack/Linear\" section."
        ));
    }

    let (verifier, challenge) = generate_pkce();
    let state = generate_state();
    let url = authorize_url(provider, &challenge, &state);

    webbrowser::open(&url).map_err(|error| format!("could not open the browser: {error}"))?;

    let callback = wait_for_redirect().await?;
    if callback.state != state {
        return Err(
            "the browser redirect didn't match this login attempt (possible CSRF) — try again"
                .to_owned(),
        );
    }

    exchange_code(provider, &callback.code, &verifier).await
}

fn authorize_url(provider: OAuthProvider, challenge: &str, state: &str) -> String {
    let mut url =
        reqwest::Url::parse(provider.endpoints().authorize_url).expect("a valid authorize URL");
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", provider.client_id())
            .append_pair("redirect_uri", &redirect_uri())
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state);

        match provider {
            // `repo` — GitHub's classic OAuth apps have no read-only scope
            // for private repos, so this is the minimum that makes private
            // repos work with this app's tools at all; this app itself
            // never calls a write endpoint. With no scope at all, GitHub's
            // API 404s (not 403) on anything the token doesn't have
            // elevated access to, to avoid leaking whether it exists.
            OAuthProvider::GitHub => {
                query.append_pair("scope", "repo");
            }
            // PKCE-enabled Slack apps can only request user-token scopes —
            // see this module's doc comment.
            OAuthProvider::Slack => {
                query.append_pair(
                    "user_scope",
                    "channels:history,groups:history,im:history,mpim:history",
                );
            }
            OAuthProvider::Linear => {
                query
                    .append_pair("response_type", "code")
                    .append_pair("scope", "read");
            }
        }
    }
    url.into()
}

/// SHA256 + base64url-no-pad, the PKCE `S256` challenge method. Base64url's
/// alphabet (`A-Za-z0-9-_`) is a subset of RFC 7636's allowed
/// `code_verifier` characters, so this doubles as the verifier's own
/// encoding with no extra transformation needed.
fn code_challenge_for(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// A random verifier long enough for RFC 7636's 43–128 character range —
/// three UUIDs' worth of raw bytes (48 bytes, 64 base64url characters) —
/// built from `uuid`'s already-secure RNG rather than adding a `rand`
/// dependency just for this.
fn generate_pkce() -> (String, String) {
    let mut raw = Vec::with_capacity(48);
    for _ in 0..3 {
        raw.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
    let challenge = code_challenge_for(&verifier);
    (verifier, challenge)
}

fn generate_state() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug)]
struct Callback {
    code: String,
    state: String,
}

/// Binds the loopback listener, waits for exactly one browser redirect (or
/// times out — a closed tab must not hang this forever), and answers it
/// with a page telling the user they can close it.
async fn wait_for_redirect() -> Result<Callback, String> {
    let listener = TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .await
        .map_err(|error| format!("could not listen on 127.0.0.1:{REDIRECT_PORT}: {error}"))?;

    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(300), listener.accept())
        .await
        .map_err(|_| "timed out waiting for the browser redirect".to_owned())?
        .map_err(|error| format!("could not accept the browser's connection: {error}"))?;

    let mut buf = [0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|error| format!("could not read the browser's request: {error}"))?;
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();

    let body = "<html><body>You can close this tab and return to Code Architect.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len(),
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;

    parse_callback_request(&request)
}

/// Parses `code`/`state` (or a reported `error`) out of the request line of
/// a raw HTTP request, e.g. `GET /callback?code=...&state=... HTTP/1.1`.
fn parse_callback_request(request: &str) -> Result<Callback, String> {
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| "empty request from the browser".to_owned())?;
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "malformed request from the browser".to_owned())?;

    let url = reqwest::Url::parse(&format!("http://127.0.0.1{path}"))
        .map_err(|error| format!("malformed redirect: {error}"))?;

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(error) = error {
        return Err(format!("the provider reported: {error}"));
    }

    Ok(Callback {
        code: code.ok_or_else(|| "no code in the redirect".to_owned())?,
        state: state.ok_or_else(|| "no state in the redirect".to_owned())?,
    })
}

async fn exchange_code(
    provider: OAuthProvider,
    code: &str,
    verifier: &str,
) -> Result<String, String> {
    let redirect_uri = redirect_uri();
    let mut params = vec![
        ("client_id", provider.client_id()),
        ("code", code),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", verifier),
    ];
    if provider == OAuthProvider::GitHub {
        params.push(("client_secret", GITHUB_CLIENT_SECRET));
    }
    if provider == OAuthProvider::Linear {
        params.push(("grant_type", "authorization_code"));
    }

    let response = reqwest::Client::new()
        .post(provider.endpoints().token_url)
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|error| format!("token request failed: {error}"))?;

    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("could not parse the token response: {error}"))?;

    if !status.is_success() {
        return Err(format!("{provider} rejected the login: {body}"));
    }

    extract_token(provider, &body)
}

/// GitHub and Linear return `access_token` at the top level; Slack's
/// PKCE/user-scoped response nests it under `authed_user` (the bot-scoped
/// `access_token` field this response also carries is for the `xoxb-`
/// token this flow deliberately doesn't request — see this module's doc
/// comment).
fn extract_token(provider: OAuthProvider, body: &Value) -> Result<String, String> {
    if let Some(error) = body["error"].as_str() {
        let description = body["error_description"].as_str().unwrap_or(error);
        return Err(format!("{provider} rejected the login: {description}"));
    }

    let token = match provider {
        OAuthProvider::GitHub | OAuthProvider::Linear => body["access_token"].as_str(),
        OAuthProvider::Slack => body["authed_user"]["access_token"].as_str(),
    };

    token
        .map(str::to_owned)
        .ok_or_else(|| format!("{provider}'s response had no access token: {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_code_challenge_matches_the_rfc_7636_test_vector() {
        // The worked example from RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifiers_have_a_valid_length_and_charset() {
        let (verifier, challenge) = generate_pkce();
        assert!(
            verifier.len() >= 43 && verifier.len() <= 128,
            "got length {}",
            verifier.len()
        );
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "got: {verifier}"
        );
        assert_eq!(challenge, code_challenge_for(&verifier));
    }

    #[test]
    fn authorize_url_includes_pkce_and_state_for_every_provider() {
        for provider in [
            OAuthProvider::GitHub,
            OAuthProvider::Slack,
            OAuthProvider::Linear,
        ] {
            let url = authorize_url(provider, "the-challenge", "the-state");
            assert!(
                url.contains("code_challenge=the-challenge"),
                "{provider}: {url}"
            );
            assert!(
                url.contains("code_challenge_method=S256"),
                "{provider}: {url}"
            );
            assert!(url.contains("state=the-state"), "{provider}: {url}");
            assert!(url.contains("redirect_uri="), "{provider}: {url}");
        }
    }

    #[test]
    fn only_slack_requests_user_scope_not_bot_scope() {
        let slack = authorize_url(OAuthProvider::Slack, "c", "s");
        assert!(slack.contains("user_scope="));
        assert!(!slack.contains("&scope="));
    }

    #[test]
    fn github_requests_the_repo_scope_so_private_repos_are_readable() {
        let github = authorize_url(OAuthProvider::GitHub, "c", "s");
        assert!(github.contains("scope=repo"), "{github}");
    }

    #[test]
    fn parses_code_and_state_from_a_real_looking_request() {
        let request = "GET /callback?code=abc123&state=xyz789 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let callback = parse_callback_request(request).unwrap();
        assert_eq!(callback.code, "abc123");
        assert_eq!(callback.state, "xyz789");
    }

    #[test]
    fn a_provider_reported_error_is_surfaced_not_treated_as_missing_code() {
        let request = "GET /callback?error=access_denied&state=xyz HTTP/1.1\r\n\r\n";
        let error = parse_callback_request(request).unwrap_err();
        assert!(error.contains("access_denied"), "got: {error}");
    }

    #[test]
    fn a_missing_code_is_an_error() {
        let request = "GET /callback?state=xyz HTTP/1.1\r\n\r\n";
        assert!(parse_callback_request(request).is_err());
    }

    #[test]
    fn extracts_githubs_and_linears_top_level_access_token() {
        let body = serde_json::json!({"access_token": "gho_abc"});
        assert_eq!(
            extract_token(OAuthProvider::GitHub, &body).unwrap(),
            "gho_abc"
        );
        assert_eq!(
            extract_token(OAuthProvider::Linear, &body).unwrap(),
            "gho_abc"
        );
    }

    #[test]
    fn extracts_slacks_nested_user_token() {
        let body = serde_json::json!({
            "ok": true,
            "access_token": "xoxb-bot-token-not-what-we-want",
            "authed_user": {"access_token": "xoxp-user-token"},
        });
        assert_eq!(
            extract_token(OAuthProvider::Slack, &body).unwrap(),
            "xoxp-user-token"
        );
    }

    #[test]
    fn a_token_response_error_field_is_reported() {
        let body = serde_json::json!({"error": "bad_verification_code"});
        let error = extract_token(OAuthProvider::GitHub, &body).unwrap_err();
        assert!(error.contains("bad_verification_code"), "got: {error}");
    }

    #[tokio::test]
    async fn wait_for_redirect_extracts_code_and_state_from_a_real_connection() {
        let handle = tokio::spawn(wait_for_redirect());
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", REDIRECT_PORT))
            .await
            .expect("connect to the loopback listener");
        stream
            .write_all(b"GET /callback?code=real-code&state=real-state HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();

        let callback = handle.await.unwrap().unwrap();
        assert_eq!(callback.code, "real-code");
        assert_eq!(callback.state, "real-state");
    }
}
