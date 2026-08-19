//! A thin wrapper over the GitHub REST API — just enough to read a PR and
//! its comments.

use reqwest::Url;
use serde_json::Value;

const API_ROOT: &str = "https://api.github.com";

pub struct GitHubClient {
    http: reqwest::Client,
    token: String,
    /// `https://api.github.com` in production; a `wiremock` server's URI in
    /// tests — the only reason this is a field rather than the `API_ROOT`
    /// constant used directly.
    base_url: String,
}

impl GitHubClient {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, API_ROOT.to_owned())
    }

    /// For tests: point requests at a local mock server instead of the real
    /// GitHub API.
    pub fn with_base_url(token: String, base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            base_url,
        }
    }

    /// One request against a path like `/repos/{owner}/{repo}/pulls/{n}`.
    pub async fn get(&self, path: &str) -> Result<Value, String> {
        let response = self
            .request(&format!("{}{path}", self.base_url))
            .send()
            .await
            .map_err(|error| format!("GitHub request failed: {error}"))?;

        Self::body(response).await
    }

    /// Every page of a list endpoint, followed via the `Link: rel="next"`
    /// response header — GitHub's real pagination mechanism for these
    /// endpoints, not a `page`/`per_page` param a caller has to guess at.
    pub async fn get_all_pages(&self, path: &str) -> Result<Vec<Value>, String> {
        let mut items = Vec::new();
        let mut url = format!("{}{path}", self.base_url);

        loop {
            let response = self
                .request(&url)
                .send()
                .await
                .map_err(|error| format!("GitHub request failed: {error}"))?;

            let next = next_link(response.headers());
            let page = Self::body(response).await?;
            match page {
                Value::Array(page) => items.extend(page),
                other => return Err(format!("expected a JSON array, got {other}")),
            }

            match next {
                Some(next_url) => url = next_url,
                None => break,
            }
        }

        Ok(items)
    }

    /// Builds a properly percent-encoded GitHub API URL from path segments
    /// and optional query params. Only needed by the contents API
    /// (`contents.rs`) — every other endpoint in this crate interpolates
    /// owner/repo/numeric ids directly into a path string, which is safe
    /// as-is, but an arbitrary repo file path can contain characters (a
    /// space, `#`, `?`, non-ASCII) that aren't safe to interpolate
    /// unescaped, and `Url::path_segments_mut`/`extend` percent-encode each
    /// segment automatically.
    pub fn build_url(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<Url, String> {
        let mut url = Url::parse(&self.base_url).map_err(|error| error.to_string())?;
        url.path_segments_mut()
            .map_err(|()| "GitHub base URL cannot be a path segment base".to_owned())?
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// Like [`Self::get`], but for a pre-built absolute URL (from
    /// [`Self::build_url`]) instead of a path string appended to
    /// `base_url`.
    pub async fn get_url(&self, url: Url) -> Result<Value, String> {
        let response = self
            .request(url.as_str())
            .send()
            .await
            .map_err(|error| format!("GitHub request failed: {error}"))?;

        Self::body(response).await
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        self.http
            .get(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "code-architect")
    }

    async fn body(response: reqwest::Response) -> Result<Value, String> {
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("could not read GitHub's response: {error}"))?;

        if !status.is_success() {
            return Err(format!("GitHub API error {status}: {text}"));
        }

        serde_json::from_str(&text)
            .map_err(|error| format!("could not parse GitHub's response: {error}"))
    }
}

/// Pulls the `rel="next"` URL out of a `Link` header, e.g.
/// `<https://api.github.com/...&page=2>; rel="next", <...>; rel="last"`.
fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get("Link")?.to_str().ok()?;

    link.split(',').find_map(|part| {
        let (url_part, rel_part) = part.split_once(';')?;
        if rel_part.contains("rel=\"next\"") {
            Some(
                url_part
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned(),
            )
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_next_link_among_several() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Link",
            "<https://api.github.com/x?page=2>; rel=\"next\", <https://api.github.com/x?page=5>; rel=\"last\""
                .parse()
                .unwrap(),
        );

        assert_eq!(
            next_link(&headers),
            Some("https://api.github.com/x?page=2".to_owned())
        );
    }

    #[test]
    fn no_link_header_means_no_next_page() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(next_link(&headers), None);
    }

    #[test]
    fn a_last_page_has_no_rel_next() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Link",
            "<https://api.github.com/x?page=1>; rel=\"prev\""
                .parse()
                .unwrap(),
        );

        assert_eq!(next_link(&headers), None);
    }
}
