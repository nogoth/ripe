//! End-to-end tests for the `ripe-run` binary: a saved pipe against a
//! mocked feed, exercised through a real child process.
//!
//! Tests use the multi-thread runtime because the wiremock server runs as a
//! tokio task on this process's runtime while the test thread blocks on the
//! child — on the single-thread flavor the server could never respond.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ripe_core::bind::{PipeParam, PipeParamKind};
use ripe_core::{Params, Pipe, save_pipe};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Two items whose document order is the reverse of their date order, so
/// the pipe's sort node observably does something.
const FEED: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Fixture</title><link>https://example.dev</link>
  <item>
    <title>Older post</title>
    <link>https://example.dev/1</link>
    <pubDate>Mon, 01 Jun 2026 10:00:00 GMT</pubDate>
    <guid>post-1</guid>
  </item>
  <item>
    <title>Newer post</title>
    <link>https://example.dev/2</link>
    <pubDate>Tue, 02 Jun 2026 10:00:00 GMT</pubDate>
    <guid>post-2</guid>
  </item>
</channel></rss>"#;

/// Temp file that removes itself (same approach as persist.rs). Names must
/// be unique per test: tests share a process id.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("ripe-run-{}-{name}", std::process::id())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// fetch_feed (url from a pipe param) -> sort by date desc -> output.
fn fixture_pipe() -> Pipe {
    let mut pipe = Pipe::new("news");
    pipe.params.push(PipeParam::new("url", PipeParamKind::Url));
    let fetch = pipe.add_node("fetch_feed", Params::new().with("url", "${url}"));
    let sort = pipe.add_node(
        "sort",
        Params::new().with("by", "pubDate").with("order", "desc"),
    );
    let output = pipe.add_node("output", Params::new());
    pipe.connect(fetch, "out", sort, "in");
    pipe.connect(sort, "out", output, "in");
    pipe
}

fn save_fixture(name: &str) -> TempFile {
    let file = TempFile::new(name);
    save_pipe(&fixture_pipe(), file.path()).unwrap();
    file
}

async fn serve(status: u16, body: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(&server)
        .await;
    server
}

fn ripe_run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ripe-run"))
        .args(args)
        .output()
        .expect("ripe-run binary spawns")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn emits_rss_to_stdout_by_default() {
    let server = serve(200, FEED).await;
    let pipe = save_fixture("rss.pipe");
    let url = format!("url={}/feed", server.uri());
    let out = ripe_run(&["-f", pipe.path().to_str().unwrap(), "--param", &url]);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "", "clean run should not warn");
    let text = stdout(&out);
    assert!(text.contains("<rss version=\"2.0\">"), "{text}");
    assert!(text.contains("<title>news</title>"), "{text}");
    let newer = text.find("Newer post").expect("newer item present");
    let older = text.find("Older post").expect("older item present");
    assert!(
        newer < older,
        "sort desc puts the newer item first:\n{text}"
    );
    // RFC 3339 in the stream, RFC 2822 on the wire.
    assert!(text.contains("Tue, 2 Jun 2026 10:00:00 +0000"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn output_flag_writes_the_same_bytes_to_a_file() {
    let server = serve(200, FEED).await;
    let pipe = save_fixture("to-file.pipe");
    let out_file = TempFile::new("to-file.out.xml");
    let url = format!("url={}/feed", server.uri());

    let to_stdout = ripe_run(&["-f", pipe.path().to_str().unwrap(), "--param", &url]);
    assert!(to_stdout.status.success(), "stderr: {}", stderr(&to_stdout));

    let to_file = ripe_run(&[
        "-f",
        pipe.path().to_str().unwrap(),
        "--param",
        &url,
        "-o",
        out_file.path().to_str().unwrap(),
    ]);
    assert!(to_file.status.success(), "stderr: {}", stderr(&to_file));
    assert!(to_file.stdout.is_empty(), "-o must silence stdout");
    let written = std::fs::read(out_file.path()).unwrap();
    assert_eq!(written, to_stdout.stdout, "file and stdout must agree");
}

#[tokio::test(flavor = "multi_thread")]
async fn format_flag_overrides_the_output_node() {
    let server = serve(200, FEED).await;
    let pipe = save_fixture("json.pipe");
    let url = format!("url={}/feed", server.uri());
    let out = ripe_run(&[
        "-f",
        pipe.path().to_str().unwrap(),
        "--param",
        &url,
        "--format",
        "json",
    ]);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let items: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let titles: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Newer post", "Older post"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_param_fails_and_names_it() {
    let pipe = save_fixture("no-param.pipe");
    let out = ripe_run(&["-f", pipe.path().to_str().unwrap()]);

    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        stderr(&out).contains("param `url` requires a value"),
        "stderr: {}",
        stderr(&out)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_param_is_a_usage_error() {
    let pipe = save_fixture("bad-param.pipe");
    let out = ripe_run(&["-f", pipe.path().to_str().unwrap(), "--param", "url"]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("expected NAME=VALUE"),
        "stderr: {}",
        stderr(&out)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_source_exits_nonzero_with_the_node_id() {
    // The only mounted response is a 500: the fetch client retries it twice
    // (~600ms of backoff) and then the node fails for real.
    let server = serve(500, "").await;
    let pipe = save_fixture("http-500.pipe");
    let url = format!("url={}/feed", server.uri());
    let out = ripe_run(&["-f", pipe.path().to_str().unwrap(), "--param", &url]);

    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "a failed run must not emit a feed");
    let err = stderr(&out);
    assert!(err.contains("node #1 (fetch_feed)"), "stderr: {err}");
    assert!(err.contains("HTTP 500"), "stderr: {err}");
    // Downstream nodes are reported too, as Unready.
    assert!(err.contains("node #2 (sort)"), "stderr: {err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn pipe_without_output_node_is_an_error() {
    let file = TempFile::new("no-output.pipe");
    save_pipe(&Pipe::new("empty"), file.path()).unwrap();
    let out = ripe_run(&["-f", file.path().to_str().unwrap()]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no output node"),
        "stderr: {}",
        stderr(&out)
    );
}
