//! Dumps every terminal event as the app sees it — for diagnosing what a
//! terminal actually delivers for chords like Shift+Enter or Ctrl+Shift+Z.
//!
//! Run: `cargo run -p dd-terminal --example keydump`
//! Press keys; each press prints a `KeyEvent` line (a `Release` line follows
//! on Windows — consoles report key-up too). `q` or `Esc` quits.

use std::io;

use ratatui::crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode},
};

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    println!("Press keys; events print below. 'q' or Esc quits.\r");
    loop {
        let event = event::read()?;
        println!("{event:?}\r");
        if let Event::Key(key) = event
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            break;
        }
    }
    disable_raw_mode()
}
