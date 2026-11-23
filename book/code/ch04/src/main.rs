//! One SQL statement against one yabiir store whose values have attributes.
//!
//! usage: sql <store-dir> <statement> [--sorted]

use attributes::computers;
use yabiir::{Bitcask, Engine, KeyOrder, Options};
use yabiir_sql::table::{self, Statement};

fn main() {
    let usage = "usage: sql <store-dir> <statement> [--sorted]";
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, text) = match args.as_slice() {
        [dir, text, ..] => (dir, text),
        _ => panic!("{usage}"),
    };
    let key_order = match args.get(2).map(String::as_str) {
        None => KeyOrder::Hashed,
        Some("--sorted") => KeyOrder::Sorted,
        Some(_) => panic!("{usage}"),
    };
    let statement = match table::parse(text) {
        Ok(statement) => statement,
        Err(e) => {
            println!("syntax error {e}");
            println!("  {text}");
            println!("  {}^", " ".repeat(e.at));
            return;
        }
    };
    let schema = computers();
    let options = Options { key_order, ..Options::default() };
    match statement {
        Statement::Insert(insert) => {
            let store = Engine::open(dir, options).expect("open the store");
            match table::insert_row(&insert, &schema, &store) {
                Ok(()) => println!("inserted 1 row"),
                Err(e) => println!("error: {e}"),
            }
        }
        Statement::Delete(_) => println!("error: this chapter has no DELETE"),
        Statement::Select(select) => {
            let store = Engine::open(dir, options.read_only()).expect("open the store");
            let plan = match table::plan(&select, &schema, store.key_order()) {
                Ok(plan) => plan,
                Err(e) => {
                    println!("error: {e}");
                    return;
                }
            };
            println!("plan:");
            for line in plan.to_string().lines() {
                println!("  {line}");
            }
            match table::execute(&plan, &schema, &store) {
                Ok(rows) => print_rows(&rows),
                Err(e) => println!("error: {e}"),
            }
        }
    }
}

fn print_rows(rows: &table::Rows) {
    let cells: Vec<Vec<String>> = rows
        .rows
        .iter()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect();
    let widths: Vec<usize> = (0..rows.columns.len())
        .map(|i| {
            cells
                .iter()
                .map(|row| row[i].len())
                .chain([rows.columns[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |row: &[String]| {
        let padded: Vec<String> =
            row.iter().zip(&widths).map(|(c, w)| format!(" {c:<w$} ")).collect();
        println!("{}", padded.join("|").trim_end());
    };
    line(&rows.columns);
    println!(
        "{}",
        widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+")
    );
    for row in &cells {
        line(row);
    }
    let n = cells.len();
    println!("({n} {})", if n == 1 { "row" } else { "rows" });
    println!("rows visited: {}", rows.visited);
}
