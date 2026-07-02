//! Fetch JSON: GET a URL, optionally select the item array with a JSONPath
//! expression, and normalize elements to items.

use crate::item::{Item, PortSpec, PortType};
use crate::module::{EvalCtx, Ins, Module, Outs};
use crate::params::{FieldKind, FieldSpec, ParamSchema, Params};

pub struct FetchJson;

#[async_trait::async_trait]
impl Module for FetchJson {
    fn kind(&self) -> &'static str {
        "fetch_json"
    }

    fn inputs(&self) -> &'static [PortSpec] {
        &[]
    }

    fn outputs(&self) -> &'static [PortSpec] {
        const OUT: &[PortSpec] = &[PortSpec::required("out", PortType::Items)];
        OUT
    }

    fn param_schema(&self) -> ParamSchema {
        ParamSchema::new(vec![
            FieldSpec::required("url", "URL", FieldKind::Url),
            FieldSpec::optional("path", "JSONPath to item array", FieldKind::Text),
        ])
    }

    async fn eval(&self, ctx: &EvalCtx, _ins: Ins, params: &Params) -> anyhow::Result<Outs> {
        let url = params
            .get_str("url")
            .ok_or_else(|| anyhow::anyhow!("param `url` is required"))?;
        let body = ctx.http.get(url).await?;
        let root: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| anyhow::anyhow!("cannot parse JSON at {url}: {e}"))?;

        let elements: Vec<serde_json::Value> = match params.get_str("path") {
            Some(path) if !path.trim().is_empty() => {
                let query = serde_json_path::JsonPath::parse(path)
                    .map_err(|e| anyhow::anyhow!("bad JSONPath `{path}`: {e}"))?;
                let nodes = query.query(&root).all();
                match nodes.as_slice() {
                    // The query landed on the array itself: take its elements.
                    [serde_json::Value::Array(arr)] => arr.clone(),
                    // The query matched individual elements.
                    _ => nodes.into_iter().cloned().collect(),
                }
            }
            _ => match root {
                serde_json::Value::Array(arr) => arr,
                other => vec![other],
            },
        };

        Ok(Outs::items(elements.into_iter().map(to_item).collect()))
    }
}

/// Objects become items directly; scalars/arrays are wrapped so nothing is
/// silently dropped.
fn to_item(value: serde_json::Value) -> Item {
    match value {
        serde_json::Value::Object(map) => Item(map),
        other => {
            let mut item = Item::new();
            item.set("value", other);
            item
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::fetch::FetchClient;
    use crate::item::PortValue;

    async fn serve(body: serde_json::Value) -> (MockServer, EvalCtx) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        (server, EvalCtx::new(FetchClient::default()))
    }

    async fn fetch(
        server: &MockServer,
        ctx: &EvalCtx,
        params: Params,
    ) -> anyhow::Result<Vec<Item>> {
        let params = params.with("url", format!("{}/data", server.uri()));
        let outs = FetchJson.eval(ctx, Ins::default(), &params).await?;
        match outs.get("out") {
            Some(PortValue::Items(items)) => Ok(items.clone()),
            other => anyhow::bail!("unexpected output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn root_array_of_objects() {
        let (server, ctx) = serve(json!([{"t": 1}, {"t": 2}])).await;
        let items = fetch(&server, &ctx, Params::new()).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].get("t"), Some(&json!(2)));
    }

    #[tokio::test]
    async fn json_path_selects_nested_array() {
        let (server, ctx) =
            serve(json!({"data": {"posts": [{"id": "a"}, {"id": "b"}], "total": 2}})).await;
        let params = Params::new().with("path", "$.data.posts");
        let items = fetch(&server, &ctx, params).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get_str("id").as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn json_path_matching_elements_directly() {
        let (server, ctx) = serve(json!({"posts": [{"id": 1}, {"id": 2}, {"id": 3}]})).await;
        let params = Params::new().with("path", "$.posts[?(@.id > 1)]");
        let items = fetch(&server, &ctx, params).await.unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn root_object_becomes_single_item_and_scalars_wrap() {
        let (server, ctx) = serve(json!({"only": "one"})).await;
        let items = fetch(&server, &ctx, Params::new()).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get_str("only").as_deref(), Some("one"));

        let (server, ctx) = serve(json!([1, 2])).await;
        let items = fetch(&server, &ctx, Params::new()).await.unwrap();
        assert_eq!(items[0].get("value"), Some(&json!(1)));
    }

    #[tokio::test]
    async fn bad_json_and_bad_path_are_clear_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
            .mount(&server)
            .await;
        let ctx = EvalCtx::new(FetchClient::default());
        let err = fetch(&server, &ctx, Params::new()).await.unwrap_err();
        assert!(err.to_string().contains("cannot parse JSON"), "{err}");

        let (server, ctx) = serve(json!([])).await;
        let params = Params::new().with("path", "$$$nonsense");
        let err = fetch(&server, &ctx, params).await.unwrap_err();
        assert!(err.to_string().contains("bad JSONPath"), "{err}");
    }
}
