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
    let harness = codeit_harness::Harness::new(&cwd, crate::providers(), crate::extensions()).await;
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
        app.reveal();
        terminal.draw(|f| {
            ui::draw(&app, f);
            app.screen.replace(f.buffer_mut().clone());
        })?;
        // While a reply is being played out, draw about 30 times a second.
        let ev = if app.revealing() {
            match tokio::time::timeout(Duration::from_millis(33), rx.recv()).await {
                Ok(ev) => ev,
                Err(_) => continue,
            }
        } else {
            rx.recv().await
        };
        let Some(ev) = ev else { break };
        app.handle(ev);
        // Apply everything already queued before drawing again (fast streams, pastes).
        while let Ok(ev) = rx.try_recv() {
            app.handle(ev);
        }
    }
    Ok(())
}
