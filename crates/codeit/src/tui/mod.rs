//! The interactive TUI.

mod app;
mod chat;
mod clipboard;
mod markdown;
mod panel;
mod review;
mod splash;
mod style;
mod ui;

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::{
    event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
};

use app::{App, AppEvent};

/// Opens the TUI, on `resume` if given.
pub async fn run(resume: Option<codeit_harness::session::Session>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let harness = codeit_harness::Harness::new(&cwd, codeit_providers::all(), crate::extensions()).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let tick_tx = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if tick_tx.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });

    let mouse = harness.config.tui.mouse != Some(false);
    let app = App::new(tx.clone(), harness, resume);
    let mut terminal = ratatui::init();
    // Before the input thread starts, so a key press reaches the splash.
    if let Err(e) = splash::run(&mut terminal) {
        ratatui::restore();
        return Err(e);
    }

    // Terminal input is read on its own thread and fed into the same queue as provider events.
    let input_tx = tx;
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.send(AppEvent::Term(ev)).is_err() {
                break;
            }
        }
    });
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    if mouse {
        let _ = execute!(std::io::stdout(), EnableMouseCapture);
    }
    let result = event_loop(&mut terminal, app, &mut rx).await;
    if mouse {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    mut app: App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| ui::draw(&app, f))?;
        let Some(ev) = rx.recv().await else { break };
        app.handle(ev);
        // Apply everything already queued before drawing again (fast streams, pastes).
        while let Ok(ev) = rx.try_recv() {
            app.handle(ev);
        }
    }
    Ok(())
}
