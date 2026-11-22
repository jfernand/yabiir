//! The table of this chapter's programs.

use yabiir_sql::table::{Domain, Schema};

/// `computers`: the key is the computer, then who made it and when it went on sale.
pub fn computers() -> Schema {
    Schema::new(&[("maker", Domain::Text), ("year", Domain::Integer)])
}
