use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Bitcask CLI: point it at a datastore directory and issue one command.
#[derive(Parser)]
#[command(name = "e", about = "A Bitcask key/value store CLI")]
struct Args {
    /// Path to the Bitcask datastore directory (created if it doesn't exist).
    dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Store a key/value pair.
    Add { key: String, value: String },
    /// Retrieve the value for a key.
    Get { key: String },
    /// Remove a key.
    Rm { key: String },
    /// List all keys.
    List,
}

fn main() {
    let args = Args::parse();

    // The `Bitcask` engine (src/api.rs) has no concrete implementation yet
    // (see docs/bitcask-implementation-plan.md, milestones 2+) — only the
    // trait and on-disk format exist so far. Wire these arms up to
    // `<impl Bitcask>::open(&args.dir, Options::default())` followed by the
    // matching trait method once that lands.
    match args.command {
        Command::Add { key, value } => {
            eprintln!(
                "add {key:?}={value:?} in {}: engine not implemented yet",
                args.dir.display()
            );
        }
        Command::Get { key } => {
            eprintln!(
                "get {key:?} in {}: engine not implemented yet",
                args.dir.display()
            );
        }
        Command::Rm { key } => {
            eprintln!(
                "rm {key:?} in {}: engine not implemented yet",
                args.dir.display()
            );
        }
        Command::List => {
            eprintln!(
                "list in {}: engine not implemented yet",
                args.dir.display()
            );
        }
    }
}
