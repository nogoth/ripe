//! ripe: the terminal pipe editor (bin `ripe`).
//!
//! The Elm Architecture (see PLAN.md): `App` is the model (`app`), input
//! becomes `Msg` (`event`), `update` is the pure transition, and `ui` renders
//! the model. `main` owns only the terminal lifecycle and the event loop.

mod app;
mod event;
mod ui;
mod update;

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::EventStream;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use ripe_core::engine::{Engine, EvalCache, EvalReport};
use ripe_core::fetch::FetchClient;
use ripe_core::format::Format;
use ripe_core::preview::Preview;
use ripe_core::{Bindings, EvalCtx, NodeStatus, Pipe, PortValue, Registry, load_pipe};

use app::{App, EvalRequest, EvalScope};
use event::Msg;

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[tokio::main]
async fn main() -> ExitCode {
    // Logs go to a file: the TUI owns the terminal. A logging failure is not
    // fatal — run without it rather than refusing to start.
    let _log_guard = match ripe_core::logging::init() {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!("warning: file logging disabled: {e:#}");
            None
        }
    };

    // Load-time failures are reported on stderr before we enter the alternate
    // screen, so the message survives the terminal teardown.
    let app = match build_app(Registry::with_builtins()) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    match run(app).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Build the initial model: open the pipe named on the command line, or start
/// on an empty one. Load warnings go to the log (the editor surfaces them once
/// the panels can); a hard load error aborts before the UI starts.
fn build_app(registry: Registry) -> anyhow::Result<App> {
    match std::env::args_os().nth(1) {
        Some(arg) => {
            let path = PathBuf::from(arg);
            let loaded = load_pipe(&path, &registry)
                .with_context(|| format!("cannot open {}", path.display()))?;
            for warning in &loaded.warnings {
                tracing::warn!(pipe = %path.display(), "{warning}");
            }
            Ok(App::with_pipe(registry, loaded.pipe, path))
        }
        None => Ok(App::new(registry)),
    }
}

/// Enter the alternate screen, run the loop, and restore the terminal no
/// matter how the loop ends. The panic hook (installed in `init_terminal`)
/// covers the crash path so a panic never leaves the user's shell in raw mode.
async fn run(mut app: App) -> anyhow::Result<()> {
    let mut terminal = init_terminal().context("initializing terminal")?;
    let result = event_loop(&mut terminal, &mut app).await;
    restore_terminal();
    result
}

async fn event_loop(terminal: &mut Tui, app: &mut App) -> anyhow::Result<()> {
    let mut events = EventStream::new();
    // 250ms drives the spinner animation and the edit-debounce countdown.
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    // Async-result plumbing (M12): eval tasks run off the render thread and
    // deliver their outcome back as a `Msg`. One HTTP client and one memo
    // cache live for the whole session — the client so conditional-request
    // caching survives across runs, the cache so editing only re-evaluates a
    // node's descendants. The cache is behind a `Mutex` because each eval
    // holds it mutably across `.await`s; aborting a superseded run drops the
    // guard and frees it for the next.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();
    let http = FetchClient::default();
    let cache = Arc::new(Mutex::new(EvalCache::new()));
    let mut in_flight: Option<JoinHandle<()>> = None;

    terminal.draw(|frame| ui::render(frame, app))?;
    while !app.should_quit {
        let msg = tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(ev)) => event::from_event(ev),
                Some(Err(e)) => return Err(e).context("reading terminal events"),
                None => break, // input stream closed
            },
            Some(result) = rx.recv() => Some(result),
            _ = ticker.tick() => Some(Msg::Tick),
        };
        if let Some(msg) = msg {
            update::update(app, msg);
        }
        // `update` is I/O-free; if it queued an eval, spawn it here.
        if let Some(request) = app.eval.pending.take() {
            let reset = std::mem::take(&mut app.eval.reset_cache);
            spawn_eval(app, request, reset, &http, &cache, &tx, &mut in_flight).await;
        }
        // Redraw every iteration: resize is handled implicitly by drawing into
        // the terminal's current size.
        terminal.draw(|frame| ui::render(frame, app))?;
    }
    if let Some(handle) = in_flight {
        handle.abort();
    }
    Ok(())
}

/// Spawn (or supersede) an eval task for `request`. Aborts whatever is still
/// running so it stops fetching and releases the cache lock, snapshots the
/// pipe + registry, and lets the task deliver its report over `tx` tagged with
/// the request's generation. Correctness against stale results rests on the
/// generation check in `update`; the abort here is the latency/resource guard.
#[allow(clippy::too_many_arguments)]
async fn spawn_eval(
    app: &App,
    request: EvalRequest,
    reset_cache: bool,
    http: &FetchClient,
    cache: &Arc<Mutex<EvalCache>>,
    tx: &UnboundedSender<Msg>,
    in_flight: &mut Option<JoinHandle<()>>,
) {
    if let Some(handle) = in_flight.take() {
        handle.abort();
    }
    if reset_cache {
        cache.lock().await.clear();
    }

    let engine = Engine::new(app.registry.clone());
    let pipe = app.pipe.clone();
    // Resolve pipe params from their declared defaults — the TUI has no
    // `--param` overrides. On error, fall back to empty bindings so the engine
    // reports per-node `${...}` errors instead of the whole run refusing.
    let bindings = Bindings::resolve(&pipe.params, &[]).unwrap_or_default();
    let ctx = EvalCtx::new(http.clone()).with_bindings(bindings);
    let cache = cache.clone();
    let tx = tx.clone();
    let EvalRequest { generation, scope } = request;

    *in_flight = Some(tokio::spawn(async move {
        // Hold the cache lock across the run *and* the preview extraction, so
        // the Output node's stream is read while it is still guaranteed the
        // one this run produced. The render thread never locks the cache; the
        // finished snapshot travels back to `App` on the message instead.
        let msg = {
            let mut guard = cache.lock().await;
            let outcome = match scope {
                EvalScope::All => engine.eval(&pipe, &mut guard, &ctx).await,
                EvalScope::UpTo(target) => {
                    engine.eval_upto(&pipe, &[target], &mut guard, &ctx).await
                }
            };
            match outcome {
                Ok(report) => {
                    let preview = extract_preview(&pipe, &report, &guard, &ctx);
                    Msg::EvalDone {
                        generation,
                        report,
                        error: None,
                        preview,
                    }
                }
                Err(e) => Msg::EvalDone {
                    generation,
                    report: EvalReport::default(),
                    error: Some(e.to_string()),
                    preview: None,
                },
            }
        };
        // The receiver only closes when the app is exiting; ignore that race.
        let _ = tx.send(msg);
    }));
}

/// Build the preview snapshot for the pipe's Output node, or `None` when this
/// run did not (re)produce one — no output node, the node failed/was skipped,
/// or a scoped "run to selected" that never reached it. The gate on the
/// current run's status is what prevents a stale, previously-cached output
/// stream from masquerading as fresh after an upstream edit broke the graph.
fn extract_preview(
    pipe: &Pipe,
    report: &EvalReport,
    cache: &EvalCache,
    ctx: &EvalCtx,
) -> Option<Preview> {
    let output_id = pipe.output_node()?;
    if !matches!(
        report.status(output_id),
        Some(NodeStatus::Ok | NodeStatus::Cached)
    ) {
        return None;
    }
    let outs = cache.output(output_id)?;
    let items = match outs.get("out") {
        Some(PortValue::Items(items)) => items,
        _ => return None,
    };
    let node = pipe.node(output_id)?;
    let format = node
        .params
        .get_str("format")
        .and_then(|s| s.parse::<Format>().ok())
        .unwrap_or(Format::Rss);
    // Age formatting uses the run's `now`, so a snapshot is deterministic
    // given the same clock the engine evaluated against.
    Some(Preview::build(items, format, &pipe.name, ctx.now))
}

/// Install the panic hook, then enter raw mode + the alternate screen.
fn init_terminal() -> anyhow::Result<Tui> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout)).map_err(Into::into)
}

/// Best-effort return to a sane terminal: errors here are ignored because the
/// caller is already on the way out (clean exit or, via the hook, a panic).
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

/// Chain terminal restoration ahead of the default panic hook so a crash
/// leaves the alternate screen and raw mode before printing its message.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original(info);
    }));
}
