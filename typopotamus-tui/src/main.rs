mod app;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::App;

#[derive(Debug, Parser)]
#[command(
    name = "typopotamus-tui",
    version,
    about = "Extract fonts from a website, multi-select by family, and download"
)]
struct Args {
    #[arg(short, long, help = "Website URL to scan immediately")]
    url: Option<String>,

    #[arg(
        short,
        long,
        default_value = "downloads",
        help = "Directory where selected fonts are saved"
    )]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let app_result = run_app(&mut terminal, args);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    app_result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, args: Args) -> Result<()> {
    let mut app = App::new(args.output, args.url);

    loop {
        app.tick();
        terminal.draw(|frame| app.draw(frame))?;

        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key_event(key);
        }
    }

    Ok(())
}
