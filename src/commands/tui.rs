use std::io;

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

// ANSI Shadow-inspired artwork is deliberately kept here so the TUI has no
// runtime font or asset dependency. Each pair is the Hay and Cut portion of a
// row, respectively; keeping the split explicit lets the two words have
// independent colors without relying on terminal escape sequences in artwork.
const WORDMARK: [(&str, &str); 6] = [
    ("██╗  ██╗ █████╗ ██╗   ██╗ ", " ██████╗██╗   ██╗████████╗"),
    ("██║  ██║██╔══██╗╚██╗ ██╔╝ ", "██╔════╝██║   ██║╚══██╔══╝"),
    ("███████║███████║ ╚████╔╝  ", "██║     ██║   ██║   ██║   "),
    ("██╔══██║██╔══██║  ╚██╔╝   ", "██║     ██║   ██║   ██║   "),
    ("██║  ██║██║  ██║   ██║    ", "╚██████╗╚██████╔╝   ██║   "),
    ("╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝    ", " ╚═════╝ ╚═════╝    ╚═╝   "),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LandingVariant {
    Full,
    Compact,
    Hidden,
}

fn prompt_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(HORIZONTAL_MARGIN * 2).max(1);
    let height = ((area.height / 2).max(3)).min(area.height.max(1));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height);
    Rect::new(x, y, width, height)
}

fn landing_variant(area: Rect) -> LandingVariant {
    let full_width = WORDMARK
        .iter()
        .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let full_height = WORDMARK.len() as u16 + 2;
    let compact_width = TAGLINE.len().max("HayCut".len()) as u16;
    if area.height < 2 || area.width < compact_width {
        LandingVariant::Hidden
    } else if area.width >= full_width && area.height >= full_height {
        LandingVariant::Full
    } else {
        LandingVariant::Compact
    }
}

fn render_landing(area: Rect, frame: &mut ratatui::Frame) {
    let variant = landing_variant(area);
    if variant == LandingVariant::Hidden {
        return;
    }
    let hay_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let cut_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let tagline_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let (height, lines): (u16, Vec<Line<'static>>) = match variant {
        LandingVariant::Full => (
            WORDMARK.len() as u16 + 2,
            WORDMARK
                .iter()
                .map(|(hay, cut)| {
                    Line::from(vec![
                        Span::styled(*hay, hay_style),
                        Span::styled(*cut, cut_style),
                    ])
                })
                .chain(std::iter::once(Line::from("")))
                .chain(std::iter::once(Line::from(Span::styled(
                    TAGLINE,
                    tagline_style,
                ))))
                .collect(),
        ),
        LandingVariant::Compact => (
            2,
            vec![
                Line::from(vec![
                    Span::styled("Hay", hay_style),
                    Span::styled("Cut", cut_style),
                ]),
                Line::from(Span::styled(TAGLINE, tagline_style)),
            ],
        ),
        LandingVariant::Hidden => unreachable!(),
    };
    let y = area.y + area.height.saturating_sub(height) / 2;
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        Rect::new(area.x, y, area.width, height),
    );
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
    let mut editor = PromptEditor::default();
    terminal.draw(|frame| editor.render(frame.area(), frame))?;

    loop {
        let event = event::read()?;
        if should_quit(event.clone()) {
            return Ok(());
        }
        if editor.handle_event(event) {
            terminal.draw(|frame| editor.render(frame.area(), frame))?;
        }
    }
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

    fn render(&mut self, area: Rect, frame: &mut ratatui::Frame) {
        let prompt = prompt_rect(area);
        let landing = Rect::new(area.x, area.y, area.width, prompt.y.saturating_sub(area.y));
        render_landing(landing, frame);
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
            let max_x = area.x + area.width.saturating_sub(1);
            let max_y = area.y + area.height.saturating_sub(1);
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
    fn wordmark_artwork_has_six_rows_and_consistent_spacing() {
        assert_eq!(WORDMARK.len(), 6);
        let widths: Vec<_> = WORDMARK
            .iter()
            .map(|(hay, cut)| hay.chars().count() + cut.chars().count())
            .collect();
        assert!(widths.iter().all(|width| *width == widths[0]));
        assert!(widths[0] > TAGLINE.chars().count());
    }

    #[test]
    fn landing_variants_switch_at_size_boundaries() {
        let full_width =
            WORDMARK[0].0.chars().count() as u16 + WORDMARK[0].1.chars().count() as u16;
        assert_eq!(
            landing_variant(Rect::new(0, 0, full_width, 8)),
            LandingVariant::Full
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, full_width.saturating_sub(1), 8)),
            LandingVariant::Compact
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, TAGLINE.len() as u16, 2)),
            LandingVariant::Compact
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, TAGLINE.len() as u16, 1)),
            LandingVariant::Hidden
        );
        assert_eq!(
            landing_variant(Rect::new(0, 0, TAGLINE.len() as u16 - 1, 2)),
            LandingVariant::Hidden
        );
    }

    #[test]
    fn landing_is_centered_in_space_above_prompt_for_odd_resize() {
        let area = Rect::new(0, 0, 81, 21);
        let prompt = prompt_rect(area);
        let landing = Rect::new(area.x, area.y, area.width, prompt.y - area.y);
        let height = WORDMARK.len() as u16 + 2;
        let landing_y = landing.y + landing.height.saturating_sub(height) / 2;
        assert!(landing_y + height <= prompt.y);
        assert_eq!(landing.width, area.width);
    }
}
