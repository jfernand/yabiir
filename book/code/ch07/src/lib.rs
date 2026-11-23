//! The table of this chapter's programs.

use yabiir::{Bitcask, Engine, KeyOrder, Options};
use yabiir_sql::table::{Datum, Domain, Schema};

/// `computers`, with a price.
pub fn schema() -> Schema {
    Schema::new(&[("maker", Domain::Text), ("year", Domain::Integer), ("price", Domain::Integer)])
}

pub fn row(key: &str, maker: &str, year: i64, price: i64) -> Vec<Datum> {
    vec![
        Datum::Text(key.into()),
        Datum::Text(maker.into()),
        Datum::Integer(year),
        Datum::Integer(price),
    ]
}

pub fn open(dir: &std::path::Path) -> Engine {
    std::fs::create_dir_all(dir).unwrap();
    Engine::open(dir, Options { key_order: KeyOrder::Sorted, ..Options::default() }).unwrap()
}
