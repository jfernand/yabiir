//! Two `yabiir-diffd` processes given the same commands must reply the same
//! and leave the same bytes on disk. If they don't, the driver or the engine
//! depends on something besides its commands (the clock, a hash seed, the
//! order of a directory listing), and a differential test against another
//! implementation would fail for reasons that aren't differences.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use yabiir_testutil::TempDir;

struct Driver {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Driver {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_yabiir-diffd"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child
            .stdin
            .take()
            .unwrap();
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .unwrap(),
        );
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, command: &str) -> String {
        writeln!(self.stdin, "{command}").unwrap();
        let mut reply = String::new();
        self.stdout
            .read_line(&mut reply)
            .unwrap();
        reply
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self
            .child
            .kill();
        let _ = self
            .child
            .wait();
    }
}

/// Every file of `dir` but the lock file, by name.
fn contents(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| {
            (
                e.file_name()
                    .into_string()
                    .unwrap(),
                fs::read(e.path()).unwrap(),
            )
        })
        .filter(|(name, _)| name != ".bitcask.lock")
        .collect()
}

fn hex(s: &str) -> String {
    s.bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The trace: commands with `{DIR}` for the store's directory.
fn trace() -> Vec<String> {
    let mut t = vec![
        r#"{"cmd":"seed_v1","dir":"{DIR}","file_id":0,"entries":[{"k":"6f6c64","v":"7631","ts":5},{"k":"676f6e65","v":"78","ts":6},{"k":"676f6e65","tombstone":true,"ts":7}]}"#.to_string(),
        r#"{"cmd":"open","dir":"{DIR}","max_file_size":200,"sorted":true}"#.to_string(),
        format!(r#"{{"cmd":"get_versioned","k":"{}"}}"#, hex("old")),
    ];
    for i in 0..40u32 {
        let k = hex(&format!("k{}", i % 7));
        let v = hex(&format!("value-{i}"));
        t.push(match i % 5 {
            0 | 1 => format!(r#"{{"cmd":"put","k":"{k}","v":"{v}","ts":{}}}"#, 100 + i),
            2 => format!(r#"{{"cmd":"delete","k":"{k}","ts":{}}}"#, 100 + i),
            3 => format!(
                r#"{{"cmd":"batch","ts":{},"ops":[{{"op":"absent","k":"{k}"}},{{"op":"put","k":"{k}","v":"{v}"}},{{"op":"put","k":"{}","v":""}}]}}"#,
                100 + i,
                hex("zz")
            ),
            _ => format!(
                r#"{{"cmd":"batch","ts":{},"ops":[{{"op":"version","k":"{k}","version":{i}}},{{"op":"delete","k":"{k}"}}]}}"#,
                100 + i
            ),
        });
        if i == 20 {
            t.push(r#"{"cmd":"merge"}"#.into());
            t.push(r#"{"cmd":"drop"}"#.into());
            t.push(r#"{"cmd":"open","dir":"{DIR}","max_file_size":200}"#.into());
            t.push(r#"{"cmd":"scan","start":null,"end":null}"#.into());
            t.push(r#"{"cmd":"close"}"#.into());
            t.push(r#"{"cmd":"open","dir":"{DIR}","max_file_size":200,"sorted":true}"#.into());
        }
    }
    t.push(r#"{"cmd":"fold"}"#.into());
    t.push(r#"{"cmd":"list_keys"}"#.into());
    t.push(format!(
        r#"{{"cmd":"scan_page","start":{{"excl":"{}"}},"end":null,"reverse":true,"limit":3}}"#,
        hex("k1")
    ));
    t.push(r#"{"cmd":"merge"}"#.into());
    t.push(r#"{"cmd":"close"}"#.into());
    t.push(r#"{"cmd":"open","dir":"{DIR}","max_file_size":200,"sorted":true}"#.into());
    t.push(r#"{"cmd":"scan_page","start":null,"end":null,"limit":100}"#.into());
    t
}

#[test]
fn two_drivers_given_the_same_commands_agree_on_replies_and_bytes() {
    let (a_dir, b_dir) = (TempDir::new(), TempDir::new());
    let (mut a, mut b) = (Driver::start(), Driver::start());
    let mut n_ok = 0;
    for (step, command) in trace()
        .iter()
        .enumerate()
    {
        let ra = a.send(
            &command.replace(
                "{DIR}",
                a_dir
                    .to_str()
                    .unwrap(),
            ),
        );
        let rb = b.send(
            &command.replace(
                "{DIR}",
                b_dir
                    .to_str()
                    .unwrap(),
            ),
        );
        assert!(!ra.contains("\"protocol\""), "step {step}: {command}\n{ra}");
        assert_eq!(ra, rb, "step {step}: {command}");
        n_ok += ra.starts_with("{\"ok\"") as usize;
        assert_eq!(contents(&a_dir), contents(&b_dir), "step {step}: {command}");
    }
    // The trace mixes failed checks in; most commands still succeed.
    assert!(n_ok > 30, "{n_ok}");
    // The v1 file was merged away, and the old key survived it.
    assert!(!contents(&a_dir).contains_key("00000000000000000000.bitcask.data"));
}
