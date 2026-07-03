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

use ripe_core::{Registry, load_pipe};

use app::App;
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
    // 250ms feeds future spinners; nothing consumes it yet beyond a redraw.
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    terminal.draw(|frame| ui::render(frame, app))?;
    while !app.should_quit {
        let msg = tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(ev)) => event::from_event(ev),
                Some(Err(e)) => return Err(e).context("reading terminal events"),
                None => break, // input stream closed
            },
            _ = ticker.tick() => Some(Msg::Tick),
        };
        if let Some(msg) = msg {
            update::update(app, msg);
        }
        // Redraw every iteration: resize is handled implicitly by drawing into
        // the terminal's current size.
        terminal.draw(|frame| ui::render(frame, app))?;
    }
    Ok(())
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
