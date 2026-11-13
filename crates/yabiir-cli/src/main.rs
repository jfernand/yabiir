//! `kv`: one command against a yabiir store.
//!
//! ```text
//! kv <dir> add <key> <value>
//! kv <dir> get <key>
//! kv <dir> rm <key>
//! kv <dir> list
//! kv <dir> merge
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use yabiir::{Bitcask, Engine, Options, now_unix};

/// Point it at a store's directory and give one command.
#[derive(Parser)]
#[command(name = "kv", about = "A command-line front end to a yabiir store")]
struct Args {
    /// The store's directory. Commands that write create it if it is missing.
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
    /// Compact the store, reclaiming the space of replaced values and
    /// deleted keys.
    Merge,
}

fn main() -> ExitCode {
    let args = Args::parse();
    run(args).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        ExitCode::FAILURE
    })
}

/// Run one command. Each invocation is its own process: it opens the store
/// (which rebuilds the keydir from the files), does one thing, and closes
/// it. Commands that only read open the store read-only, so they take no
/// lock and can run beside a writer.
fn run(args: Args) -> yabiir::Result<ExitCode> {
    let options = match args.command {
        Command::Get { .. } | Command::List => Options::default().read_only(),
        Command::Add { .. } | Command::Rm { .. } | Command::Merge => Options::default(),
    };
    let db = Engine::open(&args.dir, options)?;

    let code = match args.command {
        Command::Add { key, value } => {
            db.put(key.as_bytes(), value.as_bytes(), now_unix())?;
            ExitCode::SUCCESS
        }
        Command::Get { key } => match db.get(key.as_bytes())? {
            Some(value) => {
                println!("{}", String::from_utf8_lossy(&value));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("key not found");
                ExitCode::FAILURE
            }
        },
        Command::Rm { key } => {
            db.delete(key.as_bytes(), now_unix())?;
            ExitCode::SUCCESS
        }
        Command::List => {
            for key in db.list_keys()? {
                println!("{}", String::from_utf8_lossy(&key));
            }
            ExitCode::SUCCESS
        }
        Command::Merge => {
            db.merge()?;
            ExitCode::SUCCESS
        }
    };

    db.close()?;
    Ok(code)
}
