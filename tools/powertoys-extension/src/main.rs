// SPDX-License-Identifier: GPL-3.0-or-later
//! `nectarlink-ptrun`: PowerToys Command Palette / PowerToys Run bridge for
//! NectarLink's Local Authenticated API (`docs/api.md`).
//!
//! Usage:
//! - `nectarlink-ptrun [--data-dir <dir>] query "<search>"`
//! - `nectarlink-ptrun [--data-dir <dir>] invoke <action> [arg]`

use std::{
    env, fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiTokenFile {
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteItem {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub action: String,
    pub arg: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Query { data_dir: PathBuf, search: String },
    Invoke { data_dir: PathBuf, action: String, arg: Option<String> },
}

pub fn default_data_dir() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Nectarlink")
}

pub fn parse_args<I, S>(iter: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut data_dir = default_data_dir();
    let mut rest = Vec::new();
    let mut it = iter.into_iter().map(Into::into);
    while let Some(arg) = it.next() {
        if arg == "--data-dir" {
            let dir = it.next().ok_or_else(|| "missing value for --data-dir".to_owned())?;
            data_dir = PathBuf::from(dir);
        } else {
            rest.push(arg);
        }
    }
    let Some(sub) = rest.first().map(|s| s.as_str()) else {
        return Err("usage: nectarlink-ptrun [--data-dir <dir>] <query|invoke> ...".into());
    };
    match sub {
        "query" => {
            let search = rest.get(1).cloned().unwrap_or_default();
            Ok(Command::Query { data_dir, search })
        }
        "invoke" => {
            let action = rest
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: nectarlink-ptrun invoke <action> [arg]".to_owned())?;
            let arg = rest.get(2).cloned();
            Ok(Command::Invoke { data_dir, action, arg })
        }
        other => Err(format!("unknown subcommand: {other}")),
    }
}

pub fn read_token_file(data_dir: &Path) -> Result<ApiTokenFile, String> {
    let path = data_dir.join("api-token.json");
    let raw = fs::read_to_string(&path).map_err(|e| {
        format!(
            "can't read {} ({e}); enable the Local automation API in NectarLink Settings",
            path.display()
        )
    })?;
    serde_json::from_str(&raw).map_err(|e| format!("invalid api-token.json: {e}"))
}

pub fn build_query_items(search: &str, timeline_items: &[Value]) -> Vec<PaletteItem> {
    let q = search.trim();
    let q_lower = q.to_ascii_lowercase();

    let mut base = vec![
        PaletteItem {
            id: "send-clipboard".into(),
            title: "Send clipboard to phone".into(),
            subtitle: "Push this PC's current clipboard to your paired phone".into(),
            action: "send-clipboard".into(),
            arg: String::new(),
        },
        PaletteItem {
            id: "ring-phone".into(),
            title: "Ring phone".into(),
            subtitle: "Make your paired phone ring at full volume".into(),
            action: "ring".into(),
            arg: String::new(),
        },
        PaletteItem {
            id: "take-photo".into(),
            title: "Take photo with phone".into(),
            subtitle: "Capture a photo from your phone's camera onto this PC".into(),
            action: "camera".into(),
            arg: "photo".into(),
        },
        PaletteItem {
            id: "scan-document".into(),
            title: "Scan document with phone".into(),
            subtitle: "Scan a document with your phone's camera onto this PC".into(),
            action: "camera".into(),
            arg: "scan".into(),
        },
    ];

    if !q.is_empty() {
        if q.starts_with("http://") || q.starts_with("https://") {
            base.insert(
                0,
                PaletteItem {
                    id: "send-url".into(),
                    title: format!("Open URL on phone: {q}"),
                    subtitle: "Send link to your phone via NectarLink".into(),
                    action: "send-url".into(),
                    arg: q.to_owned(),
                },
            );
        } else {
            base.push(PaletteItem {
                id: "send-text".into(),
                title: format!("Send text to phone: {q}"),
                subtitle: "Copy text to your phone's clipboard".into(),
                action: "send-text".into(),
                arg: q.to_owned(),
            });
        }
    }

    let mut filtered: Vec<PaletteItem> = base
        .into_iter()
        .filter(|item| {
            q_lower.is_empty()
                || item.title.to_ascii_lowercase().contains(&q_lower)
                || item.subtitle.to_ascii_lowercase().contains(&q_lower)
                || item.action.starts_with("send-")
        })
        .collect();

    for entry in timeline_items.iter().take(5) {
        let title = entry.get("title").and_then(Value::as_str).unwrap_or_default();
        let peer = entry.get("peerName").and_then(Value::as_str).unwrap_or("Phone");
        let payload = entry.get("payload").and_then(Value::as_str).unwrap_or(title);
        if !title.is_empty() {
            filtered.push(PaletteItem {
                id: format!(
                    "timeline-{}",
                    entry.get("id").and_then(Value::as_str).unwrap_or("item")
                ),
                title: title.to_owned(),
                subtitle: format!("Recent from {peer}"),
                action: "send-text".into(),
                arg: payload.to_owned(),
            });
        }
    }

    filtered
}

fn api_request(
    cfg: &ApiTokenFile,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| format!("can't connect to NectarLink Local API at {addr}: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));

    let body_bytes = body.map(|v| serde_json::to_vec(v).unwrap_or_default()).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        cfg.port,
        cfg.token,
        body_bytes.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    if !body_bytes.is_empty() {
        stream.write_all(&body_bytes).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())?;

    let mut resp = String::new();
    stream.read_to_string(&mut resp).map_err(|e| e.to_string())?;
    let (_, json_body) = resp
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response from NectarLink Local API".to_owned())?;
    serde_json::from_str(json_body).map_err(|e| format!("invalid JSON from Local API: {e}"))
}

fn main() -> ExitCode {
    let cmd = match parse_args(env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nectarlink-ptrun: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cmd {
        Command::Query { data_dir, search } => {
            let timeline = read_token_file(&data_dir)
                .ok()
                .and_then(|cfg| api_request(&cfg, "GET", "/v1/timeline?limit=5", None).ok())
                .and_then(|v| v.get("items").and_then(Value::as_array).cloned())
                .unwrap_or_default();
            let items = build_query_items(&search, &timeline);
            println!("{}", serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into()));
            ExitCode::SUCCESS
        }
        Command::Invoke { data_dir, action, arg } => {
            let cfg = match read_token_file(&data_dir) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("nectarlink-ptrun: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let result = match action.as_str() {
                "ring" => api_request(&cfg, "POST", "/v1/ring", Some(&json!({}))),
                "camera" => {
                    let mode = arg.as_deref().unwrap_or("photo");
                    api_request(&cfg, "POST", "/v1/camera", Some(&json!({ "mode": mode })))
                }
                "send-clipboard" => {
                    let current = api_request(&cfg, "GET", "/v1/clipboard", None)
                        .ok()
                        .and_then(|v| v.get("text").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_default();
                    api_request(&cfg, "POST", "/v1/clipboard", Some(&json!({ "text": current })))
                }
                "send-url" => {
                    let url = arg.unwrap_or_default();
                    api_request(&cfg, "POST", "/v1/send", Some(&json!({ "url": url })))
                }
                "send-text" => {
                    let text = arg.unwrap_or_default();
                    api_request(&cfg, "POST", "/v1/send", Some(&json!({ "text": text })))
                }
                "send-file" => {
                    let path = arg.unwrap_or_default();
                    api_request(&cfg, "POST", "/v1/send", Some(&json!({ "paths": [path] })))
                }
                "notify" => {
                    let title = arg.unwrap_or_else(|| "NectarLink".into());
                    api_request(&cfg, "POST", "/v1/notify", Some(&json!({ "title": title })))
                }
                other => Err(format!("unknown action: {other}")),
            };
            match result {
                Ok(resp) => {
                    println!("{resp}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("nectarlink-ptrun: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_and_invoke_cli_arguments() {
        let q = parse_args(["--data-dir", r"C:\TestDir", "query", "ring"]).unwrap();
        assert_eq!(
            q,
            Command::Query { data_dir: PathBuf::from(r"C:\TestDir"), search: "ring".into() }
        );

        let inv = parse_args(["--data-dir", r"C:\TestDir", "invoke", "camera", "scan"]).unwrap();
        assert_eq!(
            inv,
            Command::Invoke {
                data_dir: PathBuf::from(r"C:\TestDir"),
                action: "camera".into(),
                arg: Some("scan".into()),
            }
        );

        assert!(parse_args::<[&str; 0], &str>([]).is_err());
    }

    #[test]
    fn builds_palette_items_for_empty_url_and_filtered_queries() {
        let items = build_query_items("", &[]);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].id, "send-clipboard");
        assert_eq!(items[1].id, "ring-phone");

        let url_items = build_query_items("https://nectarlink.dev", &[]);
        assert_eq!(url_items[0].id, "send-url");
        assert_eq!(url_items[0].arg, "https://nectarlink.dev");

        let ring_items = build_query_items("ring", &[]);
        assert!(ring_items.iter().any(|i| i.id == "ring-phone"));
        assert!(ring_items.iter().any(|i| i.id == "send-text"));
    }
}
