//! ripe-run: evaluate a saved pipe headlessly and emit its output stream.
//!
//! The runner is the cron-facing half of ripe: load a `.pipe` file, bind
//! `--param` overrides, evaluate the graph, and serialize the output node's
//! stream. Any node failure is a run failure — a scheduled job must not
//! silently publish a partial feed.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use ripe_core::fetch::FetchClient;
use ripe_core::format::Format;
use ripe_core::{Bindings, Engine, EvalCache, EvalCtx, NodeStatus, PortValue, Registry, load_pipe};

#[derive(Debug, Parser)]
#[command(
    name = "ripe-run",
    version,
    about = "Run a ripe pipe and emit its output"
)]
struct Args {
    /// Pipe definition file (.pipe)
    #[arg(short = 'f', long = "pipe", value_name = "FILE")]
    pipe: PathBuf,

    /// Where to write the output (default: the output node's destination
    /// param, else stdout)
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,

    /// Output format (default: the output node's format param, else rss)
    #[arg(long, value_name = "rss|atom|json|csv")]
    format: Option<Format>,

    /// Pipe parameter override, repeatable
    #[arg(long = "param", value_name = "NAME=VALUE", value_parser = parse_param)]
    params: Vec<(String, String)>,
}

/// Split a `--param` argument on the first `=`. Rejecting here makes a
/// malformed pair a clap usage error rather than a mid-run surprise.
fn parse_param(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .ok_or_else(|| format!("expected NAME=VALUE, got `{s}`"))
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime construction cannot fail");
    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> anyhow::Result<()> {
    let registry = Arc::new(Registry::with_builtins());
    let loaded = load_pipe(&args.pipe, &registry)?;
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    let pipe = loaded.pipe;

    let bindings = match Bindings::resolve(&pipe.params, &args.params) {
        Ok(bindings) => bindings,
        Err(errors) => {
            for error in &errors {
                eprintln!("error: {error}");
            }
            anyhow::bail!("{} pipe param problem(s)", errors.len());
        }
    };

    let engine = Engine::new(registry);
    let mut cache = EvalCache::new();
    let ctx = EvalCtx::new(FetchClient::default()).with_bindings(bindings);
    let report = engine.eval(&pipe, &mut cache, &ctx).await?;

    // Cached is as good as Ok (the runner starts cold, so it only appears
    // when a source deduplicates); Err and Unready both mean the stream
    // reaching the output is not the one the pipe describes.
    let mut failures = 0;
    for (id, node_report) in &report.nodes {
        if matches!(node_report.status, NodeStatus::Err | NodeStatus::Unready) {
            let kind = pipe.node(*id).map_or("?", |n| n.kind.as_str());
            let error = node_report
                .error
                .as_deref()
                .unwrap_or("did not produce output");
            eprintln!("node {id} ({kind}): {error}");
            failures += 1;
        }
    }
    if failures > 0 {
        anyhow::bail!("{failures} node(s) failed");
    }

    let output_id = pipe
        .output_node()
        .context("pipe has no output node; add one to define what to emit")?;
    let output_node = pipe
        .node(output_id)
        .expect("output_node() returns a live id");
    let outs = cache
        .output(output_id)
        .context("output node produced no output")?;
    let items = match outs.get("out") {
        Some(PortValue::Items(items)) => items,
        other => anyhow::bail!("output node carried {other:?} instead of items"),
    };

    // Flags outrank the pipe file: `--format`/`-o` beat the output node's
    // `format`/`destination` params, which beat the defaults (rss, stdout).
    let format = match args.format {
        Some(format) => format,
        None => match output_node.params.get_str("format") {
            Some(s) => s.parse().context("output node `format` param")?,
            None => Format::Rss,
        },
    };
    let destination = args.output.or_else(|| {
        output_node
            .params
            .get_str("destination")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    });

    let text = ripe_core::format::emit(format, &pipe.name, items)?;
    match destination {
        Some(path) => std::fs::write(&path, &text)
            .with_context(|| format!("cannot write {}", path.display()))?,
        None => {
            use std::io::Write;
            std::io::stdout()
                .lock()
                .write_all(text.as_bytes())
                .context("cannot write to stdout")?;
        }
    }
    Ok(())
}
