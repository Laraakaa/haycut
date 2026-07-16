use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::Paragraph;

pub fn run() -> i32 {
    match ratatui::run(|terminal| -> io::Result<()> {
        loop {
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new("Hello World!"), frame.area()))?;

            if event::poll(Duration::from_millis(250))? && should_quit(event::read()?) {
                break Ok(());
            }
        }
    }) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Terminal error: {error}");
            1
        }
    }
}

fn should_quit(event: Event) -> bool {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            ..
        }) => modifiers.contains(KeyModifiers::CONTROL),
        Event::Key(KeyEvent {
            code: KeyCode::Char('q') | KeyCode::Esc,
            ..
        }) => true,
        _ => false,
    }
}
