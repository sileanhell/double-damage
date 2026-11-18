use std::io;

use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph},
};

const BUTTONS: [&str; 2] = ["Запрос в dd-providers", "Запрос в dd-tools"];

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut menu = ListState::default().with_selected(Some(0));
    let mut output = String::from("Выберите кнопку и нажмите Enter");

    loop {
        terminal.draw(|f| draw(f, &mut menu, &output))?;

        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Up => menu.select_previous(),
            KeyCode::Down => menu.select_next(),
            KeyCode::Enter => {
                output = match menu.selected() {
                    Some(0) => dd_providers::hello(),
                    _ => dd_tools::hello(),
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            _ => {}
        }
    }
}

fn draw(f: &mut Frame, menu: &mut ListState, output: &str) {
    let [menu_area, output_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(3)]).areas(f.area());

    let items = BUTTONS.iter().map(|b| ListItem::new(format!("[ {b} ]")));
    let list = List::new(items)
        .block(Block::bordered().title(" dd — ↑/↓ выбор, Enter запрос, q выход "))
        .highlight_style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, menu_area, menu);

    f.render_widget(Paragraph::new(output).block(Block::bordered().title(" Результат ")), output_area);
}
