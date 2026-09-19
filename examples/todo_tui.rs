//! A small interactive todo tracker, built with `ratatui`, using `yabiir`
//! as its only storage layer — one key/value pair per todo, hand-encoded
//! (no `serde`) the same way `src/format/` hand-encodes entries on disk.
//!
//! Run with: `cargo run --example todo_tui [dir]` (defaults to a directory
//! under the OS temp dir, printed on startup — unlike `basic_usage.rs`,
//! that directory is *not* wiped on start, since persisting across runs is
//! the whole point of a todo tracker).
//!
//! Keys: `j`/`k` or `Up`/`Down` to move, `a` to add, `Space`/`Enter` to
//! toggle done, `d` to delete, `m` to merge, `s` to sync, `q`/`Esc` to quit.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use yabiir::{Bitcask, Engine, Options};

#[derive(Parser)]
#[command(name = "todo_tui", about = "A ratatui todo tracker backed by yabiir")]
struct Args {
    /// Datastore directory (created if missing, kept across runs).
    dir: Option<PathBuf>,
}

/// One todo item. `id` doubles as this record's yabiir key (see
/// [`todo_key`]); everything else is packed into the value by
/// [`encode_todo`]/[`decode_todo`].
struct Todo {
    id: u64,
    title: String,
    done: bool,
    created_at: u32,
}

fn todo_key(id: u64) -> Vec<u8> {
    id.to_string().into_bytes()
}

/// `[done: u8][created_at: u32 LE][title: rest of the buffer, UTF-8]` — no
/// delimiter to escape since `title` is always the last field.
fn encode_todo(todo: &Todo) -> Vec<u8> {
    let mut buf = Vec::with_capacity(5 + todo.title.len());
    buf.push(todo.done as u8);
    buf.extend_from_slice(&todo.created_at.to_le_bytes());
    buf.extend_from_slice(todo.title.as_bytes());
    buf
}

fn decode_todo(id: u64, bytes: &[u8]) -> Todo {
    let done = bytes[0] != 0;
    let created_at = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
    let title = String::from_utf8_lossy(&bytes[5..]).into_owned();
    Todo { id, title, done, created_at }
}

fn now_unix() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

enum Mode {
    Normal,
    AddingTitle(String),
}

struct App {
    db: Engine,
    todos: Vec<Todo>,
    selected: usize,
    next_id: u64,
    mode: Mode,
    status: Option<String>,
}

impl App {
    fn open(dir: PathBuf) -> yabiir::Result<Self> {
        let db = Engine::open(&dir, Options::default())?;
        let mut todos = db.fold(
            |key, value, mut acc: Vec<Todo>| {
                if let Ok(id) = std::str::from_utf8(key).unwrap_or_default().parse::<u64>() {
                    acc.push(decode_todo(id, value));
                }
                acc
            },
            Vec::new(),
        )?;
        todos.sort_by_key(|t| t.id);
        let next_id = todos.iter().map(|t| t.id).max().map_or(0, |m| m + 1);
        Ok(Self {
            db,
            todos,
            selected: 0,
            next_id,
            mode: Mode::Normal,
            status: None,
        })
    }

    fn move_selection(&mut self, delta: isize) {
        if self.todos.is_empty() {
            return;
        }
        let len = self.todos.len() as isize;
        let new = (self.selected as isize + delta).rem_euclid(len);
        self.selected = new as usize;
    }

    fn toggle_selected(&mut self) -> yabiir::Result<()> {
        if let Some(todo) = self.todos.get_mut(self.selected) {
            todo.done = !todo.done;
            self.db.put(&todo_key(todo.id), &encode_todo(todo), now_unix())?;
        }
        Ok(())
    }

    fn delete_selected(&mut self) -> yabiir::Result<()> {
        if self.selected < self.todos.len() {
            let todo = self.todos.remove(self.selected);
            self.db.delete(&todo_key(todo.id), now_unix())?;
            if self.selected >= self.todos.len() && self.selected > 0 {
                self.selected -= 1;
            }
        }
        Ok(())
    }

    fn add_todo(&mut self, title: String) -> yabiir::Result<()> {
        if title.trim().is_empty() {
            return Ok(());
        }
        let todo = Todo {
            id: self.next_id,
            title,
            done: false,
            created_at: now_unix(),
        };
        self.db.put(&todo_key(todo.id), &encode_todo(&todo), now_unix())?;
        self.next_id += 1;
        self.todos.push(todo);
        self.selected = self.todos.len() - 1;
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());

        frame.render_widget(
            Line::from(format!("yabiir todo — {} items", self.todos.len())),
            header,
        );

        let items: Vec<ListItem> = self
            .todos
            .iter()
            .map(|t| {
                let mark = if t.done { "[x]" } else { "[ ]" };
                ListItem::new(format!("{mark} {}", t.title))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Todos"))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        if !self.todos.is_empty() {
            state.select(Some(self.selected));
        }
        frame.render_stateful_widget(list, body, &mut state);

        let footer_text = match &self.mode {
            Mode::AddingTitle(buf) => format!("New todo: {buf}_"),
            Mode::Normal => {
                let help = "j/k move  a add  space/enter toggle  d delete  m merge  s sync  q quit";
                match &self.status {
                    Some(status) => format!("{help}  —  {status}"),
                    None => help.to_string(),
                }
            }
        };
        frame.render_widget(
            Paragraph::new(footer_text).style(Style::default().fg(Color::DarkGray)),
            footer,
        );
    }

    /// Returns `true` once the app should quit.
    fn handle_key(&mut self, key: KeyCode) -> yabiir::Result<bool> {
        match &mut self.mode {
            Mode::AddingTitle(buf) => match key {
                KeyCode::Enter => {
                    let title = std::mem::take(buf);
                    self.mode = Mode::Normal;
                    self.add_todo(title)?;
                }
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            },
            Mode::Normal => match key {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
                KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
                KeyCode::Char('a') => self.mode = Mode::AddingTitle(String::new()),
                KeyCode::Char(' ') | KeyCode::Enter => self.toggle_selected()?,
                KeyCode::Char('d') => self.delete_selected()?,
                KeyCode::Char('m') => {
                    self.db.merge()?;
                    self.status = Some("merged".to_string());
                }
                KeyCode::Char('s') => {
                    self.db.sync()?;
                    self.status = Some("synced".to_string());
                }
                _ => {}
            },
        }
        Ok(false)
    }
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> yabiir::Result<()> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(150))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && app.handle_key(key.code)?
        {
            return Ok(());
        }
    }
}

fn main() -> yabiir::Result<()> {
    let args = Args::parse();
    let dir = args
        .dir
        .unwrap_or_else(|| std::env::temp_dir().join("yabiir-todo-tui"));
    println!("using datastore at {}", dir.display());

    let app = App::open(dir)?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, app);
    ratatui::restore();

    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            eprintln!("error: {err}");
            Err(err)
        }
    }
}
