//! Fetch Feed: RSS 0.9x/1.0/2.0, Atom, and JSON Feed via `feed-rs`,
//! normalized to items with conventional keys.

use crate::item::{Item, PortSpec, PortType};
use crate::module::{EvalCtx, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct FetchFeed;

#[async_trait::async_trait]
impl Module for FetchFeed {
    fn kind(&self) -> &'static str {
        "fetch_feed"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        &[]
    }

    fn outputs(&self) -> &'static [PortSpec] {
        const OUT: &[PortSpec] = &[PortSpec::required("out", PortType::Items)];
        OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![FieldSpec::required("url", "Feed URL", FieldKind::Url)])
    }

    async fn eval(&self, ctx: &EvalCtx, _ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let url = params
            .get_str("url")
            .ok_or_else(|| anyhow::anyhow!("param `url` is required"))?;
        let body = ctx.http.get(url).await?;
        let feed = feed_rs::parser::parse(&body[..])
            .map_err(|e| anyhow::anyhow!("cannot parse feed at {url}: {e}"))?;
        let items = feed.entries.into_iter().map(entry_to_item).collect();
        Ok(Outs::items(items))
    }
}

/// Normalize a feed entry to the conventional item keys. Dates become
/// RFC 3339 strings; missing fields are omitted rather than set to null.
fn entry_to_item(entry: feed_rs::model::Entry) -> Item {
    let mut item = Item::new();
    if !entry.id.is_empty() {
        item.set("guid", entry.id);
    }
    if let Some(title) = entry.title {
        item.set("title", title.content);
    }
    if let Some(link) = entry.links.first() {
        item.set("link", link.href.clone());
    }
    if let Some(summary) = entry.summary {
        item.set("description", summary.content);
    }
    if let Some(body) = entry.content.and_then(|c| c.body) {
        item.set("content", body);
    }
    if let Some(date) = entry.published.or(entry.updated) {
        item.set("pubDate", date.to_rfc3339());
    }
    if let Some(author) = entry.authors.first() {
        item.set("author", author.name.clone());
    }
    item
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::fetch::FetchClient;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Test Channel</title><link>https://example.dev</link>
  <item>
    <title>First post</title>
    <link>https://example.dev/1</link>
    <description>Hello world</description>
    <pubDate>Mon, 01 Jun 2026 10:00:00 GMT</pubDate>
    <guid>post-1</guid>
  </item>
  <item>
    <title>Second post</title>
    <link>https://example.dev/2</link>
  </item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Test</title><id>urn:test</id><updated>2026-06-01T10:00:00Z</updated>
  <entry>
    <title>Atom entry</title>
    <id>urn:test:1</id>
    <link href="https://example.dev/a1"/>
    <updated>2026-06-01T10:00:00Z</updated>
    <author><name>Ada</name></author>
    <summary>An atom entry</summary>
  </entry>
</feed>"#;

    async fn serve(body: &str) -> (MockServer, EvalCtx) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/feed"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let ctx = EvalCtx::new(FetchClient::default());
        (server, ctx)
    }

    async fn fetch_items(server: &MockServer, ctx: &EvalCtx) -> anyhow::Result<Vec<Item>> {
        let params = Params::new().with("url", format!("{}/feed", server.uri()));
        let outs = FetchFeed.eval(ctx, Ins::default(), &params).await?;
        match outs.get("out") {
            Some(crate::item::PortValue::Items(items)) => Ok(items.clone()),
            other => anyhow::bail!("unexpected output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn rss2_normalizes_to_items() {
        let (server, ctx) = serve(RSS).await;
        let items = fetch_items(&server, &ctx).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get_str("title").as_deref(), Some("First post"));
        assert_eq!(
            items[0].get_str("link").as_deref(),
            Some("https://example.dev/1")
        );
        assert_eq!(
            items[0].get_str("description").as_deref(),
            Some("Hello world")
        );
        assert_eq!(items[0].get_str("guid").as_deref(), Some("post-1"));
        // RFC 2822 in, RFC 3339 out.
        assert_eq!(
            items[0].get_str("pubDate").as_deref(),
            Some("2026-06-01T10:00:00+00:00")
        );
        // Missing fields are omitted, not null.
        assert_eq!(items[1].get("pubDate"), None);
        assert_eq!(items[1].get("description"), None);
    }

    #[tokio::test]
    async fn atom_normalizes_to_items() {
        let (server, ctx) = serve(ATOM).await;
        let items = fetch_items(&server, &ctx).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get_str("title").as_deref(), Some("Atom entry"));
        assert_eq!(
            items[0].get_str("link").as_deref(),
            Some("https://example.dev/a1")
        );
        assert_eq!(items[0].get_str("author").as_deref(), Some("Ada"));
        assert_eq!(
            items[0].get_str("description").as_deref(),
            Some("An atom entry")
        );
        assert!(items[0].get_str("pubDate").is_some());
    }

    #[tokio::test]
    async fn malformed_feed_is_a_clear_error() {
        let (server, ctx) = serve("this is not xml at all {").await;
        let err = fetch_items(&server, &ctx).await.unwrap_err();
        assert!(err.to_string().contains("cannot parse feed"), "{err}");
    }

    #[tokio::test]
    async fn missing_url_param_is_an_error() {
        let ctx = EvalCtx::new(FetchClient::default());
        let err = FetchFeed
            .eval(&ctx, Ins::default(), &Params::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("`url`"), "{err}");
    }
}
