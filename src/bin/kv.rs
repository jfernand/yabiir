use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use yabiir::{Bitcask, Engine, Options};

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

fn main() -> ExitCode {
    let args = Args::parse();
    run(args).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        ExitCode::FAILURE
    })
}

/// Each invocation of this CLI is a fresh process: it opens the datastore
/// (recovering the keydir from any existing data/hint files —
/// `docs/bitcask-implementation-plan.md` §6), does exactly one operation,
/// and exits — so an `add` followed by a separate `get` invocation does
/// find the key.
fn run(args: Args) -> yabiir::Result<ExitCode> {
    let db = Engine::open(&args.dir, Options::default())?;

    let code = match args.command {
        Command::Add { key, value } => {
            db.put(key.as_bytes(), value.as_bytes())?;
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
            db.delete(key.as_bytes())?;
            ExitCode::SUCCESS
        }
        Command::List => {
            for key in db.list_keys()? {
                println!("{}", String::from_utf8_lossy(&key));
            }
            ExitCode::SUCCESS
        }
    };

    db.close()?;
    Ok(code)
}
