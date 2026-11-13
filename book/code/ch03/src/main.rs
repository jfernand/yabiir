//! One SQL query against one yabiir store, showing each step: the text, the tree the parser
//! makes of it, the plan the planner makes of the tree, and the rows the plan returns.
//!
//! usage: sql <store-dir> <query> [--sorted]
//!
//! `--sorted` opens the store with a sorted keydir, which lets the planner use a range scan.

use yabiir::sql::{self, Condition, Select};
use yabiir::{Bitcask, Engine, KeyOrder, Options};

fn main() {
    let usage = "usage: sql <store-dir> <query> [--sorted]";
    let args: Vec<String> = std::env::args()
        .skip(1)
        .collect();
    let (dir, text) = match args.as_slice() {
        [dir, text, ..] => (dir, text),
        _ => panic!("{usage}"),
    };
    let key_order = match args
        .get(2)
        .map(String::as_str)
    {
        None => KeyOrder::Hashed,
        Some("--sorted") => KeyOrder::Sorted,
        Some(_) => panic!("{usage}"),
    };

    let select = match sql::parse(text) {
        Ok(select) => select,
        Err(e) => {
            println!("syntax error {e}");
            println!("  {text}");
            println!("  {}^", " ".repeat(e.at));
            return;
        }
    };
    println!("tree: {}", tree(&select));

    let store = Engine::open(
        dir,
        Options {
            key_order,
            ..Options::default().read_only()
        },
    )
    .expect("open the store");
    let plan = sql::plan(&select, store.key_order());
    println!("plan:");
    for line in plan
        .to_string()
        .lines()
    {
        println!("  {line}");
    }

    let rows = sql::execute(&plan, &store).expect("run the plan");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let widths: Vec<usize> = (0..rows
        .columns
        .len())
        .map(|i| {
            rows.rows
                .iter()
                .map(|row| row[i].len())
                .chain([rows.columns[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<String>| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!(" {c:<w$} "))
            .collect();
        println!(
            "{}",
            padded
                .join("|")
                .trim_end()
        );
    };
    line(
        rows.columns
            .iter()
            .map(|c| c.to_string())
            .collect(),
    );
    println!(
        "{}",
        widths
            .iter()
            .map(|w| "-".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("+")
    );
    for row in &rows.rows {
        line(
            row.iter()
                .map(|f| text(f))
                .collect(),
        );
    }
    let n = rows
        .rows
        .len();
    println!("({n} {})", if n == 1 { "row" } else { "rows" });
}

/// The tree, with its strings shown as strings.
fn tree(select: &Select) -> String {
    let s = |b: &[u8]| format!("{:?}", String::from_utf8_lossy(b));
    let condition = match &select.condition {
        None => "None".to_string(),
        Some(Condition::KeyEquals(k)) => format!("Some(KeyEquals({}))", s(k)),
        Some(Condition::KeyBetween(low, high)) => {
            format!("Some(KeyBetween({}, {}))", s(low), s(high))
        }
    };
    format!(
        "Select {{ distinct: {}, fields: {:?}, condition: {condition} }}",
        select.distinct, select.fields
    )
}
