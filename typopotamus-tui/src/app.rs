use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use typopotamus_core::download::{self, DownloadReport};
use typopotamus_core::extractor::{extract_fonts_from_url, normalize_target_url};
use typopotamus_core::inspect::group_by_inferred_family;
use typopotamus_core::model::{FontFamily, FontInfo};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppMode {
    Input,
    Scanning,
    Browsing,
    Downloading,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusPane {
    Families,
    Fonts,
}

enum DownloadMessage {
    Progress {
        current: usize,
        total: usize,
        name: String,
    },
    Finished(DownloadReport),
}

pub struct App {
    pub should_quit: bool,
    url_input: String,
    output_dir: PathBuf,
    mode: AppMode,
    focus: FocusPane,
    status: String,
    fonts: Vec<FontInfo>,
    families: Vec<FontFamily>,
    selected_font_indices: HashSet<usize>,
    selected_family_index: usize,
    selected_font_row: usize,
    scan_rx: Option<Receiver<Result<Vec<FontInfo>, String>>>,
    download_rx: Option<Receiver<DownloadMessage>>,
}

impl App {
    pub fn new(output_dir: PathBuf, initial_url: Option<String>) -> Self {
        let mut app = Self {
            should_quit: false,
            url_input: initial_url.unwrap_or_default(),
            output_dir,
            mode: AppMode::Input,
            focus: FocusPane::Families,
            status: "Enter a website URL to scan for fonts".to_owned(),
            fonts: Vec::new(),
            families: Vec::new(),
            selected_font_indices: HashSet::new(),
            selected_family_index: 0,
            selected_font_row: 0,
            scan_rx: None,
            download_rx: None,
        };

        if !app.url_input.trim().is_empty() {
            app.start_scan();
        }

        app
    }

    pub fn tick(&mut self) {
        self.poll_scan_channel();
        self.poll_download_channel();
    }

    pub fn on_key_event(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match self.mode {
            AppMode::Input => self.handle_input_mode_keys(key),
            AppMode::Scanning => self.handle_busy_mode_keys(key),
            AppMode::Browsing => self.handle_browsing_mode_keys(key),
            AppMode::Downloading => self.handle_downloading_mode_keys(key),
        }
    }

    pub fn draw(&self, frame: &mut Frame) {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(frame.area());

        self.render_header(frame, vertical[0]);

        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(vertical[1]);

        self.render_url_input(frame, main[0]);

        if self.fonts.is_empty() {
            self.render_empty_state(frame, main[1]);
        } else {
            self.render_browser(frame, main[1]);
        }

        self.render_footer(frame, vertical[2]);
    }

    fn handle_input_mode_keys(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Enter => {
                if !self.url_input.trim().is_empty() {
                    self.start_scan();
                } else {
                    self.status = "URL cannot be empty".to_owned();
                }
            }
            KeyCode::Backspace => {
                self.url_input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.url_input.clear();
            }
            KeyCode::Char(character) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.url_input.push(character);
                }
            }
            _ => {}
        }
    }

    fn handle_busy_mode_keys(&mut self, key: KeyEvent) {
        if let KeyCode::Char('q') = key.code {
            self.should_quit = true;
        }
    }

    fn handle_downloading_mode_keys(&mut self, key: KeyEvent) {
        if let KeyCode::Char('q') = key.code {
            self.should_quit = true;
        }
    }

    fn handle_browsing_mode_keys(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection_down(),
            KeyCode::Char('g') => self.jump_to_top(),
            KeyCode::Char('G') => self.jump_to_bottom(),
            KeyCode::Char(' ') => self.toggle_current_selection(),
            KeyCode::Char('f') => self.toggle_current_family_selection(),
            KeyCode::Char('a') => self.toggle_select_all(),
            KeyCode::Char('d') => self.start_download(),
            KeyCode::Char('e') => self.mode = AppMode::Input,
            KeyCode::Char('r') => self.start_scan(),
            _ => {}
        }
    }

    fn poll_scan_channel(&mut self) {
        let mut clear_receiver = false;

        if let Some(receiver) = &self.scan_rx {
            match receiver.try_recv() {
                Ok(result) => {
                    clear_receiver = true;
                    match result {
                        Ok(fonts) => self.finish_scan(fonts),
                        Err(error) => {
                            self.mode = AppMode::Input;
                            self.status = format!("Scan failed: {error}");
                        }
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    clear_receiver = true;
                    self.mode = AppMode::Input;
                    self.status = "Scan worker disconnected unexpectedly".to_owned();
                }
            }
        }

        if clear_receiver {
            self.scan_rx = None;
        }
    }

    fn poll_download_channel(&mut self) {
        let mut clear_receiver = false;
        let mut disconnected = false;
        let mut messages = Vec::new();

        if let Some(receiver) = &self.download_rx {
            loop {
                match receiver.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        clear_receiver = true;
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        for message in messages {
            match message {
                DownloadMessage::Progress {
                    current,
                    total,
                    name,
                } => {
                    self.status = format!("Downloading {current}/{total}: {name}");
                }
                DownloadMessage::Finished(report) => {
                    clear_receiver = true;
                    self.finish_download(report);
                }
            }
        }

        if disconnected {
            self.mode = AppMode::Browsing;
            self.status =
                "Download worker disconnected unexpectedly; some files may be missing".to_owned();
        }

        if clear_receiver {
            self.download_rx = None;
        }
    }

    fn start_scan(&mut self) {
        let normalized_url = normalize_target_url(&self.url_input);
        self.url_input = normalized_url.clone();
        self.mode = AppMode::Scanning;
        self.status = format!("Scanning {} ...", self.url_input);

        self.fonts.clear();
        self.families.clear();
        self.selected_font_indices.clear();
        self.selected_family_index = 0;
        self.selected_font_row = 0;

        let (sender, receiver) = mpsc::channel();
        self.scan_rx = Some(receiver);

        thread::spawn(move || {
            let result = extract_fonts_from_url(&normalized_url).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn finish_scan(&mut self, fonts: Vec<FontInfo>) {
        self.fonts = fonts;
        self.families = group_by_inferred_family(&self.fonts);
        self.mode = AppMode::Browsing;
        self.focus = FocusPane::Families;
        self.selected_family_index = 0;
        self.selected_font_row = 0;

        if self.fonts.is_empty() {
            self.status = "No fonts were discovered on this website".to_owned();
        } else {
            self.status = format!(
                "Found {} fonts across {} families",
                self.fonts.len(),
                self.families.len()
            );
        }
    }

    fn start_download(&mut self) {
        let mut selected_indices: Vec<usize> = self.selected_font_indices.iter().copied().collect();
        selected_indices.sort_unstable();

        if selected_indices.is_empty() {
            self.status = "Select at least one font before downloading".to_owned();
            return;
        }

        let fonts_to_download: Vec<FontInfo> = selected_indices
            .into_iter()
            .filter_map(|index| self.fonts.get(index).cloned())
            .collect();

        let output_dir = self.output_dir.clone();
        let (sender, receiver) = mpsc::channel();
        self.download_rx = Some(receiver);
        self.mode = AppMode::Downloading;
        self.status = format!(
            "Preparing download of {} fonts to {}",
            fonts_to_download.len(),
            output_dir.display()
        );

        thread::spawn(move || {
            let report = download::download_fonts(
                &fonts_to_download,
                &output_dir,
                |current, total, font| {
                    let _ = sender.send(DownloadMessage::Progress {
                        current,
                        total,
                        name: font.name.clone(),
                    });
                },
            );
            let _ = sender.send(DownloadMessage::Finished(report));
        });
    }

    fn finish_download(&mut self, report: DownloadReport) {
        self.mode = AppMode::Browsing;

        if report.failures.is_empty() {
            self.status = format!(
                "Downloaded {}/{} fonts to {}",
                report.success_count(),
                report.attempted,
                self.output_dir.display()
            );
        } else {
            let first_failure = report.failures.first().cloned().unwrap_or_default();
            self.status = format!(
                "Downloaded {}/{} fonts ({} failed). First error: {}",
                report.success_count(),
                report.attempted,
                report.failures.len(),
                first_failure
            );
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::Families => FocusPane::Fonts,
            FocusPane::Fonts => FocusPane::Families,
        };

        self.clamp_selection();
    }

    fn move_selection_up(&mut self) {
        match self.focus {
            FocusPane::Families => {
                if self.selected_family_index > 0 {
                    self.selected_family_index -= 1;
                    self.selected_font_row = 0;
                }
            }
            FocusPane::Fonts => {
                if self.selected_font_row > 0 {
                    self.selected_font_row -= 1;
                }
            }
        }
    }

    fn move_selection_down(&mut self) {
        match self.focus {
            FocusPane::Families => {
                let last = self.families.len().saturating_sub(1);
                if self.selected_family_index < last {
                    self.selected_family_index += 1;
                    self.selected_font_row = 0;
                }
            }
            FocusPane::Fonts => {
                let current_family_len = self
                    .current_family()
                    .map_or(0, |family| family.font_indices.len());
                let last = current_family_len.saturating_sub(1);
                if self.selected_font_row < last {
                    self.selected_font_row += 1;
                }
            }
        }
    }

    fn jump_to_top(&mut self) {
        match self.focus {
            FocusPane::Families => {
                self.selected_family_index = 0;
                self.selected_font_row = 0;
            }
            FocusPane::Fonts => self.selected_font_row = 0,
        }
    }

    fn jump_to_bottom(&mut self) {
        match self.focus {
            FocusPane::Families => {
                self.selected_family_index = self.families.len().saturating_sub(1);
                self.selected_font_row = 0;
            }
            FocusPane::Fonts => {
                let last = self
                    .current_family()
                    .map_or(0, |family| family.font_indices.len().saturating_sub(1));
                self.selected_font_row = last;
            }
        }
    }

    fn toggle_current_selection(&mut self) {
        match self.focus {
            FocusPane::Families => self.toggle_current_family_selection(),
            FocusPane::Fonts => {
                if let Some(index) = self.current_font_index()
                    && !self.selected_font_indices.remove(&index)
                {
                    self.selected_font_indices.insert(index);
                }
            }
        }
    }

    fn toggle_current_family_selection(&mut self) {
        let Some(font_indices) = self
            .current_family()
            .map(|family| family.font_indices.clone())
        else {
            return;
        };

        let all_selected = font_indices
            .iter()
            .all(|font_index| self.selected_font_indices.contains(font_index));

        if all_selected {
            for font_index in font_indices {
                self.selected_font_indices.remove(&font_index);
            }
        } else {
            for font_index in font_indices {
                self.selected_font_indices.insert(font_index);
            }
        }
    }

    fn toggle_select_all(&mut self) {
        if self.fonts.is_empty() {
            return;
        }

        if self.selected_font_indices.len() == self.fonts.len() {
            self.selected_font_indices.clear();
        } else {
            self.selected_font_indices = (0..self.fonts.len()).collect();
        }
    }

    fn current_family(&self) -> Option<&FontFamily> {
        self.families.get(self.selected_family_index)
    }

    fn current_font_index(&self) -> Option<usize> {
        let family = self.current_family()?;
        family.font_indices.get(self.selected_font_row).copied()
    }

    fn clamp_selection(&mut self) {
        if self.families.is_empty() {
            self.selected_family_index = 0;
            self.selected_font_row = 0;
            return;
        }

        let max_family = self.families.len().saturating_sub(1);
        self.selected_family_index = self.selected_family_index.min(max_family);

        let max_font = self
            .current_family()
            .map_or(0, |family| family.font_indices.len().saturating_sub(1));
        self.selected_font_row = self.selected_font_row.min(max_font);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let mode_label = match self.mode {
            AppMode::Input => "Input",
            AppMode::Scanning => "Scanning",
            AppMode::Browsing => "Browsing",
            AppMode::Downloading => "Downloading",
        };

        let title = format!(
            " Font Downloader TUI | mode: {mode_label} | selected: {}/{} ",
            self.selected_font_indices.len(),
            self.fonts.len()
        );

        let paragraph = Paragraph::new(self.status.as_str())
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: true });

        frame.render_widget(paragraph, area);
    }

    fn render_url_input(&self, frame: &mut Frame, area: Rect) {
        let paragraph = Paragraph::new(self.url_input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Website URL (press Enter to scan, e to edit while browsing)"),
            )
            .style(if self.mode == AppMode::Input {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            });

        frame.render_widget(paragraph, area);

        if self.mode == AppMode::Input {
            let cursor_x = area
                .x
                .saturating_add(1)
                .saturating_add(self.url_input.len() as u16)
                .min(area.x.saturating_add(area.width.saturating_sub(2)));
            frame.set_cursor_position((cursor_x, area.y.saturating_add(1)));
        }
    }

    fn render_empty_state(&self, frame: &mut Frame, area: Rect) {
        let text = if self.mode == AppMode::Scanning {
            "Scanning website for fonts..."
        } else {
            "No fonts loaded yet. Enter a URL and press Enter."
        };

        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Fonts"))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });

        frame.render_widget(paragraph, area);
    }

    fn render_browser(&self, frame: &mut Frame, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(area);

        self.render_families(frame, columns[0]);
        self.render_fonts(frame, columns[1]);
    }

    fn render_families(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .families
            .iter()
            .map(|family| {
                let selected_count = family
                    .font_indices
                    .iter()
                    .filter(|index| self.selected_font_indices.contains(index))
                    .count();

                let marker = if selected_count == 0 {
                    "[ ]"
                } else if selected_count == family.font_indices.len() {
                    "[x]"
                } else {
                    "[-]"
                };

                ListItem::new(format!(
                    "{marker} {} ({selected_count}/{})",
                    family.name,
                    family.font_indices.len()
                ))
            })
            .collect();

        let mut state = ListState::default();
        if !self.families.is_empty() {
            state.select(Some(self.selected_family_index));
        }

        let title = if self.focus == FocusPane::Families {
            "Families (focused)"
        } else {
            "Families"
        };

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_fonts(&self, frame: &mut Frame, area: Rect) {
        let Some(family) = self.current_family() else {
            let paragraph = Paragraph::new("No family selected")
                .block(Block::default().borders(Borders::ALL).title("Fonts"));
            frame.render_widget(paragraph, area);
            return;
        };

        let items: Vec<ListItem> = family
            .font_indices
            .iter()
            .filter_map(|font_index| self.fonts.get(*font_index).map(|font| (font_index, font)))
            .map(|(font_index, font)| {
                let marker = if self.selected_font_indices.contains(font_index) {
                    "[x]"
                } else {
                    "[ ]"
                };

                let line = format!(
                    "{marker} {:>4} {:<10} {:<8} {}",
                    font.weight,
                    shrink_text(&font.style, 10),
                    shrink_text(&font.format, 8),
                    font.name
                );
                ListItem::new(line)
            })
            .collect();

        let mut state = ListState::default();
        if !family.font_indices.is_empty() {
            state.select(Some(self.selected_font_row));
        }

        let title = if self.focus == FocusPane::Fonts {
            format!("{} (focused)", family.name)
        } else {
            family.name.clone()
        };

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(Color::Green)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let help = match self.mode {
            AppMode::Input => "Type URL | Enter: scan | Ctrl+u: clear URL | q: quit",
            AppMode::Scanning => "Scanning... please wait | q: quit",
            AppMode::Browsing => {
                "Tab: switch pane | ↑/↓: move | Space: toggle | f: family toggle | a: toggle all | d: download | r: rescan | e: edit URL | q: quit"
            }
            AppMode::Downloading => "Downloading selected fonts... | q: quit",
        };

        let footer = Paragraph::new(format!(
            "{} | Output directory: {}",
            help,
            self.output_dir.display()
        ))
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .wrap(Wrap { trim: true });

        frame.render_widget(footer, area);
    }
}

fn shrink_text(input: &str, max_width: usize) -> String {
    if input.chars().count() <= max_width {
        return input.to_owned();
    }

    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let mut output = String::new();
    for character in input.chars().take(max_width.saturating_sub(3)) {
        output.push(character);
    }
    output.push_str("...");
    output
}
