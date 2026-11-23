//! The oldest computer, three ways over one store: the calculus formula read
//! directly, the algebra with a difference, and the algebra Codd's reduction
//! produces, with a division. Each relation is a sorted set of tuples.
//!
//! usage: oldest <empty-dir>

use std::collections::BTreeSet;

use yabiir::{Bitcask, Engine, Options};

type Row = (String, u32);
type Pair = (Row, Row);

const ROWS: [(&str, u32); 5] = [
    ("apple-ii", 1977),
    ("ibm-pc", 1981),
    ("bbc-micro", 1981),
    ("zx-spectrum", 1982),
    ("c64", 1982),
];

fn store_relation(store: &Engine) -> BTreeSet<Row> {
    store
        .fold(
            |k, v, mut rows: BTreeSet<Row>| {
                let key = String::from_utf8(k.to_vec()).unwrap();
                let year = std::str::from_utf8(v).unwrap().parse().unwrap();
                rows.insert((key, year));
                rows
            },
            BTreeSet::new(),
        )
        .unwrap()
}

fn product(a: &BTreeSet<Row>, b: &BTreeSet<Row>) -> BTreeSet<Pair> {
    a.iter().flat_map(|x| b.iter().map(move |y| (x.clone(), y.clone()))).collect()
}

/// { t : t ∈ store ∧ ∀u (u ∈ store → t.value ≤ u.value) }
fn calculus(r: &BTreeSet<Row>) -> BTreeSet<Row> {
    r.iter().filter(|t| r.iter().all(|u| t.1 <= u.1)).cloned().collect()
}

/// store − π_s(σ_{s.value > u.value}(ρ_s(store) × ρ_u(store)))
fn difference(r: &BTreeSet<Row>) -> BTreeSet<Row> {
    let not_oldest: BTreeSet<Row> =
        product(r, r).into_iter().filter(|(s, u)| s.1 > u.1).map(|(s, _)| s).collect();
    r.difference(&not_oldest).cloned().collect()
}

/// `t[(1,2,3,4) ÷ (1,2)]s`: the left halves paired with every row of `s`.
fn divide(t: &BTreeSet<Pair>, s: &BTreeSet<Row>) -> BTreeSet<Row> {
    let lefts: BTreeSet<&Row> = t.iter().map(|(l, _)| l).collect();
    lefts
        .into_iter()
        .filter(|l| s.iter().all(|u| t.contains(&((*l).clone(), u.clone()))))
        .cloned()
        .collect()
}

fn names(rows: &BTreeSet<Row>) -> String {
    let v: Vec<String> = rows.iter().map(|(k, y)| format!("({k}, {y})")).collect();
    v.join(" ")
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: oldest <empty-dir>");
    let store = Engine::open(&dir, Options::default()).unwrap();
    for (key, year) in ROWS {
        store.put(key.as_bytes(), year.to_string().as_bytes(), yabiir::now_unix()).unwrap();
    }
    let r = store_relation(&store);

    println!("calculus     {}", names(&calculus(&r)));
    println!("difference   {}", names(&difference(&r)));
    println!();
    println!("Codd's reduction");
    let (s1, s2) = (r.clone(), r.clone());
    println!("step 2  S1 = R, S2 = R                 {} and {} tuples", s1.len(), s2.len());
    let s = product(&s1, &s2);
    println!("step 3  S = S1 x S2                    {} tuples", s.len());
    let t2: BTreeSet<Pair> = s.into_iter().filter(|(a, b)| a.1 <= b.1).collect();
    println!("step 4  T2 = S[2 <= 4]                 {} tuples", t2.len());
    let t1 = divide(&t2, &s2);
    println!("step 5  T1 = T2[(1,2,3,4) / (1,2)]S2   {} tuple", t1.len());
    println!("step 6  T = T1[1,2]                    {}", names(&t1));
}
