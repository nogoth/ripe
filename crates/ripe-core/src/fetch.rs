//! Shared HTTP fetching for source modules: one pooled client with
//! timeouts, bounded retries, a response-size cap, and conditional-request
//! caching (ETag / Last-Modified) so live editing doesn't hammer remotes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub timeout: Duration,
    /// Additional attempts after the first, on connect errors and 5xx.
    pub retries: u32,
    /// Bodies larger than this are rejected, not truncated.
    pub max_bytes: usize,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            retries: 2,
            max_bytes: 10 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("HTTP {status} from {url}")]
    Status { status: u16, url: String },
    #[error("response from {url} exceeds size cap of {limit} bytes")]
    TooLarge { url: String, limit: usize },
}

struct CachedResponse {
    etag: Option<String>,
    last_modified: Option<String>,
    body: Arc<[u8]>,
}

/// Cheap to clone; all clones share the connection pool and the
/// conditional-request cache.
#[derive(Clone)]
pub struct FetchClient {
    client: reqwest::Client,
    config: Arc<FetchConfig>,
    cache: Arc<Mutex<HashMap<String, CachedResponse>>>,
}

impl Default for FetchClient {
    fn default() -> Self {
        Self::new(FetchConfig::default())
    }
}

impl FetchClient {
    pub fn new(config: FetchConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(concat!("ripe/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("static client config cannot fail");
        Self {
            client,
            config: Arc::new(config),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// GET `url` and return the body. Sends conditional headers when we
    /// have a cached copy; a 304 answers from cache.
    pub async fn get(&self, url: &str) -> Result<Arc<[u8]>, FetchError> {
        let mut attempt = 0;
        loop {
            match self.get_once(url).await {
                Ok(body) => return Ok(body),
                Err(e) if attempt < self.config.retries && is_retryable(&e) => {
                    attempt += 1;
                    tracing::debug!(url, attempt, error = %e, "retrying fetch");
                    tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn get_once(&self, url: &str) -> Result<Arc<[u8]>, FetchError> {
        let (etag, last_modified) = {
            let cache = self.cache.lock().expect("fetch cache poisoned");
            match cache.get(url) {
                Some(c) => (c.etag.clone(), c.last_modified.clone()),
                None => (None, None),
            }
        };
        let mut request = self.client.get(url);
        if let Some(etag) = &etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(lm) = &last_modified {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, lm);
        }

        let response = request.send().await?;
        let status = response.status();

        if status == reqwest::StatusCode::NOT_MODIFIED {
            let cache = self.cache.lock().expect("fetch cache poisoned");
            if let Some(cached) = cache.get(url) {
                tracing::debug!(url, "304, serving cached body");
                return Ok(cached.body.clone());
            }
            // A 304 without a cached copy means we never sent conditional
            // headers; treat it as a server error.
            return Err(FetchError::Status {
                status: 304,
                url: url.to_string(),
            });
        }
        if !status.is_success() {
            return Err(FetchError::Status {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }

        let new_etag = header_string(&response, reqwest::header::ETAG);
        let new_last_modified = header_string(&response, reqwest::header::LAST_MODIFIED);

        let mut body: Vec<u8> = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > self.config.max_bytes {
                return Err(FetchError::TooLarge {
                    url: url.to_string(),
                    limit: self.config.max_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        let body: Arc<[u8]> = body.into();

        if new_etag.is_some() || new_last_modified.is_some() {
            let mut cache = self.cache.lock().expect("fetch cache poisoned");
            cache.insert(
                url.to_string(),
                CachedResponse {
                    etag: new_etag,
                    last_modified: new_last_modified,
                    body: body.clone(),
                },
            );
        }
        Ok(body)
    }
}

fn header_string(
    response: &reqwest::Response,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    response
        .headers()
        .get(name)?
        .to_str()
        .ok()
        .map(str::to_string)
}

fn is_retryable(error: &FetchError) -> bool {
    match error {
        FetchError::Http(e) => e.is_connect() || e.is_timeout(),
        FetchError::Status { status, .. } => *status >= 500,
        FetchError::TooLarge { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn small_client() -> FetchClient {
        FetchClient::new(FetchConfig {
            max_bytes: 64,
            ..FetchConfig::default()
        })
    }

    #[tokio::test]
    async fn plain_get() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/feed"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&server)
            .await;
        let body = FetchClient::default()
            .get(&format!("{}/feed", server.uri()))
            .await
            .unwrap();
        assert_eq!(&*body, b"hello");
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let err = FetchClient::default()
            .get(&format!("{}/gone", server.uri()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, FetchError::Status { status: 404, .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn size_cap_rejects_large_bodies() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 1000]))
            .mount(&server)
            .await;
        let err = small_client()
            .get(&format!("{}/big", server.uri()))
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::TooLarge { .. }), "{err}");
    }

    #[tokio::test]
    async fn retries_recover_from_transient_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("recovered"))
            .with_priority(5)
            .mount(&server)
            .await;
        let body = FetchClient::default()
            .get(&format!("{}/flaky", server.uri()))
            .await
            .unwrap();
        assert_eq!(&*body, b"recovered");
    }

    #[tokio::test]
    async fn etag_flow_serves_304_from_cache() {
        let server = MockServer::start().await;
        // Revalidation: only matches once the client sends If-None-Match.
        Mock::given(method("GET"))
            .and(header("If-None-Match", "\"v1\""))
            .respond_with(ResponseTemplate::new(304))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("cached body")
                    .insert_header("ETag", "\"v1\""),
            )
            .with_priority(5)
            .expect(1)
            .mount(&server)
            .await;

        let client = FetchClient::default();
        let url = format!("{}/etag", server.uri());
        let first = client.get(&url).await.unwrap();
        let second = client.get(&url).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(&*second, b"cached body");
    }
}
