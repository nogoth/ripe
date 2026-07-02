//! Fetch CSV: GET a URL and turn each record into an item, with the header
//! row supplying field names. Values stay strings; coercion is downstream's
//! job (Sort is numeric-aware, Filter coerces per rule).

use crate::item::{Item, PortSpec, PortType};
use crate::module::{EvalCtx, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct FetchCsv;

#[async_trait::async_trait]
impl Module for FetchCsv {
    fn kind(&self) -> &'static str {
        "fetch_csv"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        &[]
    }

    fn outputs(&self) -> &'static [PortSpec] {
        const OUT: &[PortSpec] = &[PortSpec::required("out", PortType::Items)];
        OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![FieldSpec::required("url", "CSV URL", FieldKind::Url)])
    }

    async fn eval(&self, ctx: &EvalCtx, _ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let url = params
            .get_str("url")
            .ok_or_else(|| anyhow::anyhow!("param `url` is required"))?;
        let body = ctx.http.get(url).await?;
        let mut reader = csv::Reader::from_reader(&body[..]);
        let headers = reader
            .headers()
            .map_err(|e| anyhow::anyhow!("cannot read CSV header at {url}: {e}"))?
            .clone();
        let mut items = Vec::new();
        for record in reader.records() {
            let record = record.map_err(|e| anyhow::anyhow!("bad CSV at {url}: {e}"))?;
            let mut item = Item::new();
            for (header, value) in headers.iter().zip(record.iter()) {
                item.set(header, value);
            }
            items.push(item);
        }
        Ok(Outs::items(items))
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::fetch::FetchClient;
    use crate::item::PortValue;

    async fn serve(body: &str) -> (MockServer, EvalCtx) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data.csv"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        (server, EvalCtx::new(FetchClient::default()))
    }

    async fn fetch(server: &MockServer, ctx: &EvalCtx) -> anyhow::Result<Vec<Item>> {
        let params = Params::new().with("url", format!("{}/data.csv", server.uri()));
        let outs = FetchCsv.eval(ctx, Ins::default(), &params).await?;
        match outs.get("out") {
            Some(PortValue::Items(items)) => Ok(items.clone()),
            other => anyhow::bail!("unexpected output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn headers_become_field_names() {
        let (server, ctx) = serve("title,link\nHello,https://x.dev/1\nBye,https://x.dev/2\n").await;
        let items = fetch(&server, &ctx).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get_str("title").as_deref(), Some("Hello"));
        assert_eq!(items[1].get_str("link").as_deref(), Some("https://x.dev/2"));
    }

    #[tokio::test]
    async fn empty_body_yields_no_items() {
        let (server, ctx) = serve("").await;
        assert_eq!(fetch(&server, &ctx).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn ragged_rows_are_a_clear_error() {
        let (server, ctx) = serve("a,b\n1,2\n1,2,3\n").await;
        let err = fetch(&server, &ctx).await.unwrap_err();
        assert!(err.to_string().contains("bad CSV"), "{err}");
    }
}
