//! The commands `yabiir-diffd` reads and the replies it writes, one JSON
//! object per line.
//!
//! Commands, by their `cmd` field:
//!
//! ```text
//! open        {dir, max_file_size, sync_on_put, sorted, read_only}  -> null
//! close | drop | merge | sync                                       -> null
//! put         {k, v, ts}                                            -> null
//! delete      {k, ts}                                               -> null
//! batch       {ops: [{op: put|delete|absent|exists|version, k, v?, version?}], ts}
//!                                                                   -> version | null
//! get         {k}                                                   -> value | null
//! get_versioned {k}                                                 -> {value, version} | null
//! list_keys                                                         -> [key]
//! fold                                                              -> [[key, value]]
//! scan        {start, end}                                          -> [[key, value]]
//! scan_page   {start, end, reverse, limit}                          -> [[key, value, version]]
//! seed_v1     {dir, file_id, entries: [{k, v, tombstone, ts}]}      -> null
//! ```
//!
//! A bound is `null` (unbounded), `{"incl": key}` or `{"excl": key}`. Keys
//! and values are lowercase hex. `drop` releases the store without the
//! fsync `close` does, like a process that stops.
//!
//! Replies are `{"ok": …}`, `{"err": "<yabiir::Error variant>"}` (with
//! `index` for `ConditionFailed` and `kind` for `Io`), or `{"protocol":
//! "<message>"}` for a command the driver couldn't run at all.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use yabiir::Error;

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Open {
        dir: PathBuf,
        max_file_size: u64,
        #[serde(default)]
        sync_on_put: bool,
        #[serde(default)]
        sorted: bool,
        #[serde(default)]
        read_only: bool,
    },
    Close,
    Drop,
    Put {
        k: Hex,
        v: Hex,
        ts: u32,
    },
    Delete {
        k: Hex,
        ts: u32,
    },
    Batch {
        ops: Vec<BatchOp>,
        ts: u32,
    },
    Get {
        k: Hex,
    },
    GetVersioned {
        k: Hex,
    },
    ListKeys,
    Fold,
    Scan {
        start: Option<KeyBound>,
        end: Option<KeyBound>,
    },
    ScanPage {
        start: Option<KeyBound>,
        end: Option<KeyBound>,
        #[serde(default)]
        reverse: bool,
        limit: usize,
    },
    Merge,
    Sync,
    SeedV1 {
        dir: PathBuf,
        file_id: u32,
        entries: Vec<V1Entry>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BatchOp {
    Put { k: Hex, v: Hex },
    Delete { k: Hex },
    Absent { k: Hex },
    Exists { k: Hex },
    Version { k: Hex, version: u64 },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyBound {
    Incl(Hex),
    Excl(Hex),
}

#[derive(Deserialize)]
pub struct V1Entry {
    pub k: Hex,
    #[serde(default)]
    pub v: Hex,
    #[serde(default)]
    pub tombstone: bool,
    pub ts: u32,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Reply {
    Ok {
        ok: Value,
    },
    Err {
        err: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    Protocol {
        protocol: String,
    },
}

impl Reply {
    pub fn protocol(message: String) -> Self {
        Reply::Protocol { protocol: message }
    }

    pub fn from_error(e: &Error) -> Self {
        let (name, index, kind) = match e {
            Error::Io(io) => ("Io", None, Some(format!("{:?}", io.kind()))),
            Error::AlreadyLocked => ("AlreadyLocked", None, None),
            Error::EmptyKey => ("EmptyKey", None, None),
            Error::KeyTooLarge => ("KeyTooLarge", None, None),
            Error::ValueTooLarge => ("ValueTooLarge", None, None),
            Error::ReadOnly => ("ReadOnly", None, None),
            Error::Unordered => ("Unordered", None, None),
            Error::ConditionFailed { index } => ("ConditionFailed", Some(*index), None),
            Error::BatchTooLarge => ("BatchTooLarge", None, None),
            other => {
                return Reply::Err {
                    err: format!("{other:?}"),
                    index: None,
                    kind: None,
                };
            }
        };
        Reply::Err {
            err: name.into(),
            index,
            kind,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Version(u64),
    Bytes(Hex),
    Versioned { value: Hex, version: u64 },
    Keys(Vec<Hex>),
    Pairs(Vec<(Hex, Hex)>),
    Page(Vec<(Hex, Hex, u64)>),
}

/// Bytes, as a lowercase hex string on the wire.
#[derive(Default)]
pub struct Hex(pub Vec<u8>);

impl From<Vec<u8>> for Hex {
    fn from(bytes: Vec<u8>) -> Self {
        Hex(bytes)
    }
}

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(
            self.0
                .len()
                * 2,
        );
        for &b in &self.0 {
            out.push(DIGITS[(b >> 4) as usize] as char);
            out.push(DIGITS[(b & 0xf) as usize] as char);
        }
        s.serialize_str(&out)
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() % 2 != 0 {
            return Err(serde::de::Error::custom("odd-length hex"));
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map(Hex)
            .map_err(serde::de::Error::custom)
    }
}
