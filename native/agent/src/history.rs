use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transcript {
    hermes_rust_version: u32,
    messages: Vec<Value>,
}

pub struct History {
    path: PathBuf,
    _lock: File,
}

impl History {
    pub fn open(path: PathBuf) -> Result<(Self, Vec<Value>), String> {
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|err| format!("cannot open history lock: {err}"))?;
        lock.try_lock()
            .map_err(|err| format!("cannot lock history: {err}"))?;

        let messages = match fs::read(&path) {
            Ok(bytes) => {
                let transcript: Transcript = serde_json::from_slice(&bytes)
                    .map_err(|err| format!("invalid Rust transcript: {err}"))?;
                if transcript.hermes_rust_version != 1 {
                    return Err("unsupported Rust transcript version".into());
                }
                transcript.messages
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(format!("cannot read history: {err}")),
        };
        Ok((Self { path, _lock: lock }, messages))
    }

    pub fn save(&self, messages: &[Value]) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut pending = tempfile::NamedTempFile::new_in(parent)
            .map_err(|err| format!("cannot create history temporary file: {err}"))?;
        let transcript = serde_json::json!({
            "hermes_rust_version": 1,
            "messages": messages,
        });
        serde_json::to_writer(pending.as_file_mut(), &transcript)
            .map_err(|err| format!("cannot encode history: {err}"))?;
        pending
            .write_all(b"\n")
            .and_then(|_| pending.as_file().sync_all())
            .map_err(|err| format!("cannot write history: {err}"))?;
        pending
            .persist(&self.path)
            .map_err(|err| format!("cannot replace history: {err}"))?;
        Ok(())
    }
}
