//! From [`Select`] to a [`Plan`], and from a plan to rows.
//!
//! A plan is a tree of operators. The leaves are the three ways to read the
//! store — every row with `fold`, one row with `get`, a range of rows with
//! [`Engine::scan`] — and the inner nodes reshape what the leaves produce:
//! keep the rows a condition holds for, keep some fields, drop repeats.
//! Execution pushes rows from a leaf up through its parents, one row at a
//! time; only `Distinct` remembers anything.

use std::collections::HashSet;
use std::fmt;
use std::ops::Bound;

use super::ast::{Condition, Fields, Literal, Select};
use yabiir::{Bitcask, Engine, KeyOrder, Result};

/// A tree of operators that answers one query against one store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Every row: one `fold` over the store. σ_true.
    FullScan,
    /// At most one row: one keydir lookup and one read.
    Get(Vec<u8>),
    /// The rows from `low` to `high`, both included, in key order: one
    /// descent into a sorted keydir and a walk.
    RangeScan(Vec<u8>, Vec<u8>),
    /// The rows of the input the condition holds for. σ_condition.
    Filter(Condition, Box<Plan>),
    /// One field of each row of the input. π_field.
    Project(Fields, Box<Plan>),
    /// The rows of the input, each once.
    Distinct(Box<Plan>),
}

/// Decide how to answer `select` against a store whose keydir keeps its
/// keys in `order`.
pub fn plan(select: &Select, order: KeyOrder) -> Plan {
    // Where the rows come from: the selection decides.
    let mut plan = match &select.condition {
        None => Plan::FullScan,
        Some(Condition::KeyEquals(key)) => Plan::Get(key.clone()),
        Some(Condition::KeyBetween(low, high)) if order == KeyOrder::Sorted => {
            Plan::RangeScan(low.clone(), high.clone())
        }
        // A range over a hashed keydir: visit every row and test each key.
        Some(condition) => Plan::Filter(condition.clone(), Box::new(Plan::FullScan)),
    };
    // What each row keeps: `*` keeps both fields, which is every row as is.
    if select.fields != Fields::All {
        plan = Plan::Project(select.fields, Box::new(plan));
    }
    // DISTINCT, unless it can't change anything: a result that carries the
    // key has no two equal rows, and a lookup returns at most one row.
    let at_most_one_row = matches!(select.condition, Some(Condition::KeyEquals(_)));
    if select.distinct
        && !select
            .fields
            .includes_key()
        && !at_most_one_row
    {
        plan = Plan::Distinct(Box::new(plan));
    }
    plan
}

/// A query's answer: the names of its columns, and its rows in the order
/// the plan produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rows {
    pub columns: Vec<&'static str>,
    pub rows: Vec<Vec<Vec<u8>>>,
}

/// Run `plan` against `store`.
pub fn execute(plan: &Plan, store: &Engine) -> Result<Rows> {
    let mut rows = Vec::new();
    run(plan, store, &mut |row| rows.push(row))?;
    Ok(Rows {
        columns: plan
            .columns()
            .to_vec(),
        rows,
    })
}

/// Push every row `plan` produces into `emit`. A leaf's rows are
/// `[key, value]`; `Project` is the only node that changes a row's shape,
/// and the planner puts it above every `Filter`, which tests the key.
fn run(plan: &Plan, store: &Engine, emit: &mut dyn FnMut(Vec<Vec<u8>>)) -> Result<()> {
    match plan {
        Plan::FullScan => store.fold(
            |key, value, ()| emit(vec![key.to_vec(), value.to_vec()]),
            (),
        ),
        Plan::Get(key) => {
            if let Some(value) = store.get(key)? {
                emit(vec![key.clone(), value]);
            }
            Ok(())
        }
        Plan::RangeScan(low, high) => {
            let range = store.scan(Bound::Included(low), Bound::Included(high))?;
            for (key, value) in range {
                emit(vec![key, value]);
            }
            Ok(())
        }
        Plan::Filter(condition, input) => run(input, store, &mut |row| {
            if condition.holds_for(&row[0]) {
                emit(row);
            }
        }),
        Plan::Project(fields, input) => run(input, store, &mut |mut row| {
            match fields {
                Fields::All => {}
                Fields::Key => row.truncate(1),
                Fields::Value => {
                    row.swap_remove(0);
                }
            }
            emit(row);
        }),
        Plan::Distinct(input) => {
            let mut seen = HashSet::new();
            run(input, store, &mut |row| {
                if seen.insert(row.clone()) {
                    emit(row);
                }
            })
        }
    }
}

impl Plan {
    /// The names of the columns this plan's rows have.
    pub fn columns(&self) -> &'static [&'static str] {
        match self {
            Plan::FullScan | Plan::Get(_) | Plan::RangeScan(..) => Fields::All.columns(),
            Plan::Filter(_, input) | Plan::Distinct(input) => input.columns(),
            Plan::Project(fields, _) => fields.columns(),
        }
    }

    fn explain(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        let indent = "  ".repeat(depth);
        match self {
            Plan::FullScan => writeln!(f, "{indent}FullScan (fold: every row)"),
            Plan::Get(key) => writeln!(f, "{indent}Get {} (get: one lookup)", Literal(key)),
            Plan::RangeScan(low, high) => writeln!(
                f,
                "{indent}RangeScan {} .. {} (scan: sorted keydir)",
                Literal(low),
                Literal(high)
            ),
            Plan::Filter(condition, input) => {
                writeln!(f, "{indent}Filter {condition}")?;
                input.explain(f, depth + 1)
            }
            Plan::Project(fields, input) => {
                writeln!(
                    f,
                    "{indent}Project {}",
                    fields
                        .columns()
                        .join(", ")
                )?;
                input.explain(f, depth + 1)
            }
            Plan::Distinct(input) => {
                writeln!(f, "{indent}Distinct")?;
                input.explain(f, depth + 1)
            }
        }
    }
}

/// The plan as an indented tree, root first — what `EXPLAIN` prints.
impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.explain(f, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use std::path::Path;
    use yabiir::Options;
    use yabiir_testutil::TempDir;

    const COMPUTERS: [(&str, &str); 5] = [
        ("apple-ii", "1977"),
        ("ibm-pc", "1981"),
        ("bbc-micro", "1981"),
        ("zx-spectrum", "1982"),
        ("c64", "1982"),
    ];

    fn store(dir: &Path, key_order: KeyOrder) -> Engine {
        let store = Engine::open(
            dir,
            Options {
                key_order,
                ..Options::default()
            },
        )
        .unwrap();
        for (key, value) in COMPUTERS {
            store
                .put(key.as_bytes(), value.as_bytes(), yabiir::now_unix())
                .unwrap();
        }
        store
    }

    fn planned(text: &str, order: KeyOrder) -> Plan {
        plan(&parse(text).unwrap(), order)
    }

    fn sorted(mut rows: Vec<Vec<Vec<u8>>>) -> Vec<Vec<String>> {
        rows.sort();
        rows.into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|f| String::from_utf8(f).unwrap())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_condition_picks_the_access_path() {
        use Plan::*;
        let b = |s: &str| {
            s.as_bytes()
                .to_vec()
        };
        assert_eq!(planned("SELECT *", KeyOrder::Hashed), FullScan);
        assert_eq!(
            planned("SELECT * WHERE key = 'c64'", KeyOrder::Hashed),
            Get(b("c64"))
        );
        assert_eq!(
            planned("SELECT * WHERE key BETWEEN 'a' AND 'c'", KeyOrder::Sorted),
            RangeScan(b("a"), b("c"))
        );
        assert_eq!(
            planned("SELECT * WHERE key BETWEEN 'a' AND 'c'", KeyOrder::Hashed),
            Filter(Condition::KeyBetween(b("a"), b("c")), Box::new(FullScan))
        );
    }

    #[test]
    fn distinct_is_planned_only_where_it_can_remove_a_row() {
        use Plan::*;
        let keeps_distinct = |text| matches!(planned(text, KeyOrder::Hashed), Distinct(_));
        assert!(keeps_distinct("SELECT DISTINCT value"));
        assert!(!keeps_distinct("SELECT DISTINCT *"));
        assert!(!keeps_distinct("SELECT DISTINCT key"));
        assert!(!keeps_distinct("SELECT DISTINCT value WHERE key = 'c64'"));
        assert!(keeps_distinct(
            "SELECT DISTINCT value WHERE key BETWEEN 'a' AND 'z'"
        ));
    }

    #[test]
    fn every_plan_answers_like_the_definition_on_both_orders() {
        let queries = [
            "SELECT *",
            "SELECT key",
            "SELECT value",
            "SELECT DISTINCT value",
            "SELECT DISTINCT *",
            "SELECT * WHERE key = 'zx-spectrum'",
            "SELECT value WHERE key = 'amiga'",
            "SELECT * WHERE key BETWEEN 'b' AND 'c64'",
            "SELECT DISTINCT value WHERE key BETWEEN 'a' AND 'j'",
            "SELECT key WHERE key BETWEEN 'z' AND 'a'",
        ];
        for order in [KeyOrder::Hashed, KeyOrder::Sorted] {
            let dir = TempDir::new();
            let store = store(&dir, order);
            for text in queries {
                let select = parse(text).unwrap();
                let got = execute(&plan(&select, order), &store).unwrap();

                // The definition, straight from the tree: σ, then π, then
                // DISTINCT, over every row.
                let mut want: Vec<Vec<Vec<u8>>> = Vec::new();
                for (key, value) in COMPUTERS {
                    let (key, value) = (
                        key.as_bytes()
                            .to_vec(),
                        value
                            .as_bytes()
                            .to_vec(),
                    );
                    if select
                        .condition
                        .as_ref()
                        .is_some_and(|c| !c.holds_for(&key))
                    {
                        continue;
                    }
                    let row = match select.fields {
                        Fields::All => vec![key, value],
                        Fields::Key => vec![key],
                        Fields::Value => vec![value],
                    };
                    if !select.distinct || !want.contains(&row) {
                        want.push(row);
                    }
                }
                assert_eq!(
                    got.columns,
                    select
                        .fields
                        .columns(),
                    "{text} ({order:?})"
                );
                assert_eq!(sorted(got.rows), sorted(want), "{text} ({order:?})");
            }
        }
    }

    #[test]
    fn a_range_scan_returns_its_rows_in_key_order() {
        let dir = TempDir::new();
        let store = store(&dir, KeyOrder::Sorted);
        let got = execute(
            &planned("SELECT key WHERE key BETWEEN 'a' AND 'j'", KeyOrder::Sorted),
            &store,
        )
        .unwrap();
        let keys: Vec<_> = got
            .rows
            .iter()
            .map(|row| row[0].as_slice())
            .collect();
        assert_eq!(keys, [&b"apple-ii"[..], b"bbc-micro", b"c64", b"ibm-pc"]);
    }

    #[test]
    fn explain_prints_the_tree_root_first() {
        let text = planned(
            "SELECT DISTINCT value WHERE key BETWEEN 'a' AND 'j'",
            KeyOrder::Hashed,
        )
        .to_string();
        let lines: Vec<_> = text
            .lines()
            .map(str::trim_end)
            .collect();
        assert_eq!(
            lines,
            [
                "Distinct",
                "  Project value",
                "    Filter key BETWEEN 'a' AND 'j'",
                "      FullScan (fold: every row)",
            ]
        );
    }
}
