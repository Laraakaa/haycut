use std::io;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

const HORIZONTAL_MARGIN: u16 = 1;
const TAGLINE: &str = "efficient coding harness";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_SHA: &str = env!("HAYCUT_BUILD_SHA");

const ANSI_COMPACT: [(&str, &str); 3] = [
    ("██  ██  ▄▄▄  ", "▄▄ ▄▄ ▄█████ ▄▄ ▄▄ ▄▄▄▄▄▄ "),
    ("██████ ██▀██ ", "▀███▀ ██     ██ ██   ██   "),
    ("██  ██ ██▀██ ", "  █   ▀█████ ▀███▀   ██  "),
];
const LOGO_CANVAS_HEIGHT: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LandingVariant {
    Full,
    Compact,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutMode {
    Landing,
    Chat,
}

fn prompt_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(HORIZONTAL_MARGIN * 2).max(1);
    let height = ((area.height / 2).max(3)).min(area.height.max(1));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height);
    Rect::new(x, y, width, height)
}

fn landing_variant(area: Rect) -> LandingVariant {
    let full_width = ANSI_COMPACT
        .iter()
        .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let full_height = 1 + LOGO_CANVAS_HEIGHT + 1;
    let compact_width = "HayCut".chars().count() as u16;
    if area.height < 3 || area.width < compact_width {
        LandingVariant::Hidden
    } else if area.width >= full_width && area.height >= full_height {
        LandingVariant::Full
    } else {
        LandingVariant::Compact
    }
}

fn ansi_logo_lines() -> Vec<Line<'static>> {
    let hay_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let cut_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    ANSI_COMPACT
        .iter()
        .map(|(hay, cut)| {
            Line::from(vec![
                Span::styled(*hay, hay_style),
                Span::styled(*cut, cut_style),
            ])
        })
        .collect()
}

fn render_branding(area: Rect, frame: &mut ratatui::Frame) {
    if area.height == 0 {
        return;
    }
    let metadata_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    frame.render_widget(
        Paragraph::new(Span::styled(metadata(true), metadata_style)).alignment(Alignment::Right),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let variant = landing_variant(area);
    if variant == LandingVariant::Hidden {
        return;
    }
    let tagline_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    match variant {
        LandingVariant::Full => {
            frame.render_widget(
                Paragraph::new(ansi_logo_lines()).alignment(Alignment::Center),
                Rect::new(area.x, area.y + 1, area.width, LOGO_CANVAS_HEIGHT),
            );
            frame.render_widget(
                Paragraph::new(Span::styled(TAGLINE, tagline_style)).alignment(Alignment::Center),
                Rect::new(area.x, area.y + 1 + LOGO_CANVAS_HEIGHT, area.width, 1),
            );
        }
        LandingVariant::Compact => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "HayCut",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Center),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
            frame.render_widget(
                Paragraph::new(Span::styled(TAGLINE, tagline_style)).alignment(Alignment::Center),
                Rect::new(area.x, area.y + 2, area.width, 1),
            );
        }
        LandingVariant::Hidden => unreachable!(),
    }
}

fn metadata(include_sha: bool) -> String {
    if include_sha {
        format!("v{VERSION} · {BUILD_SHA}")
    } else {
        format!("v{VERSION}")
    }
}

fn render_header(area: Rect, frame: &mut ratatui::Frame) {
    if area.height == 0 {
        return;
    }
    let full_width = (6 + 1 + metadata(true).chars().count()) as u16;
    let right = metadata(area.width >= full_width);
    let gap = area
        .width
        .saturating_sub((6 + right.chars().count()) as u16) as usize;
    let line = Line::from(vec![
        Span::styled("HayCut", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            right,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(area.width as usize),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

pub fn run() -> i32 {
    match ratatui::run(|terminal| -> io::Result<()> {
        let mut stdout = io::stdout();
        let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
        if enhanced {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )?;
        }

        let result = run_editor(terminal);
        let restore = if enhanced {
            execute!(stdout, PopKeyboardEnhancementFlags)
        } else {
            Ok(())
        };
        result.and(restore)
    }) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Terminal error: {error}");
            1
        }
    }
}

fn run_editor(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    let mut app = App::default();
    terminal.draw(|frame| app.render(frame.area(), frame))?;

    loop {
        let event = if app.pending.is_some() {
            if event::poll(Duration::from_millis(120))? {
                event::read()?
            } else {
                app.tick();
                terminal.draw(|frame| app.render(frame.area(), frame))?;
                continue;
            }
        } else {
            event::read()?
        };
        if should_quit(event.clone()) {
            return Ok(());
        }
        if app.handle_event(event) {
            terminal.draw(|frame| app.render(frame.area(), frame))?;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TimelineEntry {
    User(String),
    Assistant(String),
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoEvent {
    ResponseStarted,
    AssistantMessage,
}

#[derive(Default)]
struct DemoDriver {
    scenario: usize,
}

impl DemoDriver {
    fn start(&mut self) -> DemoEvent {
        self.scenario = 0;
        DemoEvent::ResponseStarted
    }

    fn advance(&mut self) -> Option<DemoEvent> {
        if self.scenario == 0 {
            self.scenario = 1;
            Some(DemoEvent::AssistantMessage)
        } else {
            None
        }
    }
}

struct InFlightTurn {
    animation_frame: usize,
}

struct App {
    layout: LayoutMode,
    editor: PromptEditor,
    timeline: Vec<TimelineEntry>,
    pending: Option<InFlightTurn>,
    demo: DemoDriver,
}

impl Default for App {
    fn default() -> Self {
        Self {
            layout: LayoutMode::Landing,
            editor: PromptEditor::default(),
            timeline: Vec::new(),
            pending: None,
            demo: DemoDriver::default(),
        }
    }
}

impl App {
    fn handle_event(&mut self, event: Event) -> bool {
        if matches!(event, Event::Resize(_, _)) {
            return true;
        }
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind != crossterm::event::KeyEventKind::Press {
            return false;
        }
        if key.code == KeyCode::Char('1')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
        {
            return self.advance_demo();
        }
        if key.code == KeyCode::Enter
            && !key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return self.submit();
        }
        self.editor.handle_event(Event::Key(key))
    }

    fn submit(&mut self) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let prompt = self.editor.text();
        if prompt.trim().is_empty() {
            return false;
        }
        self.timeline.push(TimelineEntry::User(prompt));
        self.editor.reset();
        self.layout = LayoutMode::Chat;
        let event = self.demo.start();
        self.apply_demo_event(event)
    }

    fn advance_demo(&mut self) -> bool {
        let Some(event) = self.demo.advance() else {
            return false;
        };
        self.apply_demo_event(event)
    }

    fn apply_demo_event(&mut self, event: DemoEvent) -> bool {
        match event {
            DemoEvent::ResponseStarted => {
                self.timeline.push(TimelineEntry::Pending);
                self.pending = Some(InFlightTurn { animation_frame: 0 });
            }
            DemoEvent::AssistantMessage => {
                if let Some(last) = self.timeline.last_mut() {
                    if !matches!(last, TimelineEntry::Pending) {
                        return false;
                    }
                    *last = TimelineEntry::Assistant("hello world".to_string());
                    self.pending = None;
                } else {
                    return false;
                }
            }
        }
        true
    }

    fn tick(&mut self) -> bool {
        let Some(turn) = self.pending.as_mut() else {
            return false;
        };
        turn.animation_frame = (turn.animation_frame + 1) % SPINNER.len();
        true
    }

    fn render(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        let prompt = prompt_rect(area);
        match self.layout {
            LayoutMode::Landing => {
                let landing =
                    Rect::new(area.x, area.y, area.width, prompt.y.saturating_sub(area.y));
                render_branding(landing, frame);
            }
            LayoutMode::Chat => {
                let header = Rect::new(area.x, area.y, area.width, area.height.min(2));
                render_header(header, frame);
                let viewport_y = header.y + header.height;
                let viewport = Rect::new(
                    area.x,
                    viewport_y,
                    area.width,
                    prompt.y.saturating_sub(viewport_y),
                );
                render_chat(viewport, &self.timeline, self.pending.as_ref(), frame);
            }
        }
        self.editor.render(prompt, frame);
    }
}

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    text.lines()
        .flat_map(|line| {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                return vec![String::new()];
            }
            chars
                .chunks(width)
                .map(|chunk| chunk.iter().collect())
                .collect()
        })
        .collect()
}

fn timeline_lines(
    entries: &[TimelineEntry],
    width: usize,
    pending: Option<&InFlightTurn>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let user_style = Style::default().fg(Color::Yellow);
    let assistant_style = Style::default().fg(Color::Green);
    let pending_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    for (index, entry) in entries.iter().enumerate() {
        let (label, text, style) = match entry {
            TimelineEntry::User(text) => ("You  ".to_string(), text.clone(), user_style),
            TimelineEntry::Assistant(text) => {
                ("HayCut  ".to_string(), text.clone(), assistant_style)
            }
            TimelineEntry::Pending => (
                "HayCut  ".to_string(),
                format!(
                    "{} thinking",
                    SPINNER[pending.map_or(0, |turn| turn.animation_frame)]
                ),
                pending_style,
            ),
        };
        let wrapped = wrap_text(&text, width.saturating_sub(label.chars().count()));
        for (line_index, content) in wrapped.into_iter().enumerate() {
            let prefix = if line_index == 0 {
                label.clone()
            } else {
                "       ".to_string()
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(content, style),
            ]));
        }
        if index + 1 < entries.len() {
            lines.push(Line::from(""));
        }
    }
    lines
}

fn render_chat(
    area: Rect,
    entries: &[TimelineEntry],
    pending: Option<&InFlightTurn>,
    frame: &mut ratatui::Frame,
) {
    if area.height == 0 {
        return;
    }
    let lines = timeline_lines(entries, area.width.saturating_sub(2) as usize, pending);
    let scroll = lines.len().saturating_sub(area.height as usize) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

struct PromptEditor {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    vertical_scroll: usize,
}

impl Default for PromptEditor {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            vertical_scroll: 0,
        }
    }
}

impl PromptEditor {
    fn reset(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.vertical_scroll = 0;
    }

    fn handle_event(&mut self, event: Event) -> bool {
        let Event::Key(key) = event else {
            return matches!(event, Event::Resize(_, _));
        };
        if key.kind != crossterm::event::KeyEventKind::Press {
            return false;
        }
        match key.code {
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.insert(ch)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => self.newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            KeyCode::Home => {
                self.cursor_col = 0;
                true
            }
            KeyCode::End => {
                self.cursor_col = self.lines[self.cursor_line].chars().count();
                true
            }
            _ => false,
        }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn insert(&mut self, ch: char) -> bool {
        let byte = self.lines[self.cursor_line]
            .char_indices()
            .nth(self.cursor_col)
            .map_or(self.lines[self.cursor_line].len(), |(i, _)| i);
        self.lines[self.cursor_line].insert(byte, ch);
        self.cursor_col += 1;
        true
    }

    fn newline(&mut self) -> bool {
        let byte = self.lines[self.cursor_line]
            .char_indices()
            .nth(self.cursor_col)
            .map_or(self.lines[self.cursor_line].len(), |(i, _)| i);
        let rest = self.lines[self.cursor_line].split_off(byte);
        self.lines.insert(self.cursor_line + 1, rest);
        self.cursor_line += 1;
        self.cursor_col = 0;
        true
    }

    fn backspace(&mut self) -> bool {
        if self.cursor_col > 0 {
            let start = self.lines[self.cursor_line]
                .char_indices()
                .nth(self.cursor_col - 1)
                .map(|(i, _)| i)
                .unwrap();
            let end = self.lines[self.cursor_line]
                .char_indices()
                .nth(self.cursor_col)
                .map_or(self.lines[self.cursor_line].len(), |(i, _)| i);
            self.lines[self.cursor_line].replace_range(start..end, "");
            self.cursor_col -= 1;
            true
        } else if self.cursor_line > 0 {
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            self.lines[self.cursor_line].push_str(&current);
            true
        } else {
            false
        }
    }

    fn delete(&mut self) -> bool {
        let len = self.lines[self.cursor_line].chars().count();
        if self.cursor_col < len {
            let _ = self.right();
            let _ = self.backspace();
            true
        } else if self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            true
        } else {
            false
        }
    }

    fn left(&mut self) -> bool {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            true
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            true
        } else {
            false
        }
    }
    fn right(&mut self) -> bool {
        if self.cursor_col < self.lines[self.cursor_line].chars().count() {
            self.cursor_col += 1;
            true
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
            true
        } else {
            false
        }
    }
    fn up(&mut self) -> bool {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self
                .cursor_col
                .min(self.lines[self.cursor_line].chars().count());
            true
        } else {
            false
        }
    }
    fn down(&mut self) -> bool {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self
                .cursor_col
                .min(self.lines[self.cursor_line].chars().count());
            true
        } else {
            false
        }
    }

    fn render(&mut self, prompt: Rect, frame: &mut ratatui::Frame) {
        let Rect {
            x,
            y,
            width,
            height,
        } = prompt;
        let inner_width = width.saturating_sub(2).max(1) as usize;
        let rows = self.visual_rows(inner_width);
        let content_height = height.saturating_sub(2).max(1) as usize;
        let cursor = rows
            .iter()
            .position(|row| {
                row.0 == self.cursor_line && self.cursor_col >= row.1 && self.cursor_col <= row.2
            })
            .unwrap_or(0);
        if cursor < self.vertical_scroll {
            self.vertical_scroll = cursor;
        }
        if cursor >= self.vertical_scroll + content_height {
            self.vertical_scroll = cursor + 1 - content_height;
        }
        let visible: Vec<Line<'_>> = rows
            .iter()
            .skip(self.vertical_scroll)
            .take(content_height)
            .map(|row| Line::from(row.3.clone()))
            .collect();
        let block = Block::default().borders(Borders::ALL);
        frame.render_widget(
            Paragraph::new(visible).block(block),
            Rect::new(x, y, width, height),
        );
        let cursor_x = rows.get(cursor).map_or(0, |row| {
            row.3
                .chars()
                .take(self.cursor_col.saturating_sub(row.1))
                .map(|ch| ch.width().unwrap_or(0))
                .sum::<usize>()
        }) as u16;
        if width > 0 && height > 0 {
            let max_x = prompt.x + prompt.width.saturating_sub(1);
            let max_y = prompt.y + prompt.height.saturating_sub(1);
            frame.set_cursor_position((
                (x + 1 + cursor_x.min(inner_width as u16)).min(max_x),
                (y + 1 + cursor.saturating_sub(self.vertical_scroll) as u16).min(max_y),
            ));
        }
    }

    fn visual_rows(&self, width: usize) -> Vec<(usize, usize, usize, String)> {
        let mut rows = Vec::new();
        for (line_no, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                rows.push((line_no, 0, 0, String::new()));
                continue;
            }
            let mut start = 0;
            while start < chars.len() {
                let mut end = start;
                let mut cells = 0;
                while end < chars.len() {
                    let w = chars[end].width().unwrap_or(0);
                    if end > start && cells + w > width {
                        break;
                    }
                    cells += w;
                    end += 1;
                }
                rows.push((line_no, start, end, chars[start..end].iter().collect()));
                start = end;
            }
        }
        rows
    }
}

fn should_quit(event: Event) -> bool {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => true,
        Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            ..
        }) => modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn editor() -> PromptEditor {
        PromptEditor::default()
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn inserts_and_deletes_unicode_by_character() {
        let mut e = editor();
        e.handle_event(key(KeyCode::Char('é')));
        e.handle_event(key(KeyCode::Char('x')));
        assert_eq!(e.lines, vec!["éx"]);
        e.handle_event(key(KeyCode::Backspace));
        assert_eq!(e.lines, vec!["é"]);
        e.handle_event(key(KeyCode::Backspace));
        assert_eq!(e.lines, vec![""]);
    }

    #[test]
    fn shift_enter_newlines_but_plain_enter_does_not() {
        let mut e = editor();
        assert!(!e.handle_event(key(KeyCode::Enter)));
        assert!(e.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT
        ))));
        assert_eq!(e.lines.len(), 2);
    }

    #[test]
    fn q_is_text_and_exit_keys_are_reserved() {
        let mut e = editor();
        assert!(e.handle_event(key(KeyCode::Char('q'))));
        assert_eq!(e.lines[0], "q");
        assert!(should_quit(key(KeyCode::Esc)));
        assert!(should_quit(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))));
    }

    #[test]
    fn visual_rows_wrap_and_keep_explicit_lines() {
        let mut e = editor();
        e.lines = vec!["abcd".into(), "é".into()];
        let rows = e.visual_rows(3);
        assert_eq!(
            rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>(),
            vec!["abc", "d", "é"]
        );
    }

    #[test]
    fn navigation_and_delete_join_lines() {
        let mut e = editor();
        e.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
        )));
        e.handle_event(key(KeyCode::Char('a')));
        e.handle_event(key(KeyCode::Up));
        e.handle_event(key(KeyCode::Delete));
        assert_eq!(e.lines, vec!["a"]);
        e.handle_event(key(KeyCode::Home));
        e.handle_event(key(KeyCode::Right));
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn cursor_scroll_is_bounded_to_visible_rows() {
        let mut e = editor();
        e.lines = (0..10).map(|n| n.to_string()).collect();
        let rows = e.visual_rows(10);
        let cursor = rows.len() - 1;
        let viewport = 3;
        e.vertical_scroll = (cursor + 1 - viewport).min(rows.len() - viewport);
        assert_eq!(e.vertical_scroll, 7);
    }

    #[test]
    fn non_empty_enter_starts_chat_and_preserves_multiline_message() {
        let mut app = App::default();
        app.handle_event(key(KeyCode::Char('a')));
        app.handle_event(modified_key(KeyCode::Enter, KeyModifiers::SHIFT));
        app.handle_event(key(KeyCode::Char('b')));
        assert!(app.handle_event(key(KeyCode::Enter)));
        assert_eq!(app.layout, LayoutMode::Chat);
        assert_eq!(app.editor.text(), "");
        assert_eq!(
            app.timeline,
            vec![TimelineEntry::User("a\nb".into()), TimelineEntry::Pending]
        );
        assert!(app.pending.is_some());
    }

    #[test]
    fn whitespace_enter_is_ignored_and_shift_enter_still_inserts_newline() {
        let mut app = App::default();
        app.handle_event(key(KeyCode::Char(' ')));
        assert!(!app.handle_event(key(KeyCode::Enter)));
        assert_eq!(app.layout, LayoutMode::Landing);
        app.handle_event(modified_key(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.editor.lines, vec![" ".to_string(), String::new()]);
    }

    #[test]
    fn spinner_only_advances_while_pending() {
        let mut app = App::default();
        assert!(!app.tick());
        app.editor.insert('x');
        app.submit();
        assert!(app.tick());
        assert_eq!(app.pending.as_ref().unwrap().animation_frame, 1);
        app.advance_demo();
        assert!(!app.tick());
    }

    #[test]
    fn ctrl_one_completes_demo_and_preserves_pending_draft() {
        let mut app = App::default();
        app.editor.insert('x');
        app.submit();
        app.editor.insert('d');
        assert!(!app.handle_event(key(KeyCode::Enter)));
        assert_eq!(app.editor.text(), "d");
        assert!(app.handle_event(modified_key(KeyCode::Char('1'), KeyModifiers::CONTROL,)));
        assert!(app.pending.is_none());
        assert_eq!(app.editor.text(), "d");
        assert_eq!(
            app.timeline.last(),
            Some(&TimelineEntry::Assistant("hello world".into()))
        );
        assert!(!app.handle_event(modified_key(KeyCode::Char('1'), KeyModifiers::CONTROL,)));
    }

    #[test]
    fn completed_turn_can_start_another_demo() {
        let mut app = App::default();
        app.editor.insert('x');
        app.submit();
        app.advance_demo();
        app.editor.insert('y');
        assert!(app.handle_event(key(KeyCode::Enter)));
        assert!(app.pending.is_some());
        assert_eq!(app.timeline.len(), 4);
        assert_eq!(app.timeline[2], TimelineEntry::User("y".into()));
    }

    #[test]
    fn ansi_logo_has_three_rows_and_stable_spacing() {
        assert_eq!(ANSI_COMPACT.len(), 3);
        let widths: Vec<_> = ANSI_COMPACT
            .iter()
            .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
            .collect();
        let canvas_width = *widths.iter().max().unwrap();
        assert!(widths.iter().all(|width| *width <= canvas_width));
        assert!(widths[0] > TAGLINE.chars().count());
        assert_eq!(ansi_logo_lines().len(), LOGO_CANVAS_HEIGHT as usize);
    }

    #[test]
    fn landing_variants_switch_at_size_boundaries() {
        let full_width = ANSI_COMPACT
            .iter()
            .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
            .max()
            .unwrap_or(0) as u16;
        assert_eq!(
            landing_variant(Rect::new(0, 0, full_width, 5)),
            LandingVariant::Full
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, full_width.saturating_sub(1), 5)),
            LandingVariant::Compact
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, "HayCut".chars().count() as u16, 3)),
            LandingVariant::Compact
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, "HayCut".chars().count() as u16 - 1, 3)),
            LandingVariant::Hidden
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, "HayCut".chars().count() as u16, 2)),
            LandingVariant::Hidden
        );
    }

    #[test]
    fn metadata_is_versioned_and_fixed_width() {
        assert_eq!(metadata(false), "v0.1.0");
        assert!(metadata(true).contains(" · "));
        assert_eq!(BUILD_SHA.chars().count(), 8);
    }

    #[test]
    fn landing_is_centered_in_space_above_prompt_for_odd_resize() {
        let area = Rect::new(0, 0, 81, 21);
        let prompt = prompt_rect(area);
        let landing = Rect::new(area.x, area.y, area.width, prompt.y - area.y);
        let content = Rect::new(landing.x, landing.y + 1, landing.width, landing.height - 1);
        let height = ANSI_COMPACT.len() as u16 + 2;
        let branding_y = content.y + content.height.saturating_sub(height) / 2;
        assert!(branding_y + height <= prompt.y);
        assert_eq!(landing.width, area.width);
    }
}
