use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn serve(status: &str, body: &str) -> (String, thread::JoinHandle<(String, String)>) {
    let (url, server) = serve_many(&[(status, body)]);
    (
        url,
        thread::spawn(move || server.join().unwrap().pop().unwrap()),
    )
}

type Requests = thread::JoinHandle<Vec<(String, String)>>;

fn serve_many(replies: &[(&str, &str)]) -> (String, Requests) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let responses: Vec<String> = replies.iter().map(|(status, body)| format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )).collect();
    let handle = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")?
                            .trim()
                            .parse()
                            .ok()
                    })
                    .unwrap();
                let mut request = vec![0; length];
                reader.read_exact(&mut request).unwrap();
                socket.write_all(response.as_bytes()).unwrap();
                (headers, String::from_utf8(request).unwrap())
            })
            .collect()
    });
    (url, handle)
}

#[test]
fn stdin_query_reaches_the_provider_and_prints_the_answer() {
    let (url, server) = serve("200 OK", r#"{"choices":[{"message":{"content":"回答"}}]}"#);
    let mut cli = Command::new(env!("CARGO_BIN_EXE_hermes-agent-rs"))
        .args(["--base-url", &url, "-m", "test-model", "--query-file", "-"])
        .env("OPENAI_API_KEY", "test-key")
        .env("NO_PROXY", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    cli.stdin
        .take()
        .unwrap()
        .write_all("日本語 \"quotes\"\r\n次の行".as_bytes())
        .unwrap();
    let output = cli.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim_end(), "回答");
    let (headers, body) = server.join().unwrap();
    assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key")
    );
    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        request,
        serde_json::json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "日本語 \"quotes\"\n次の行"}]
        })
    );
}

#[test]
fn provider_failure_returns_a_nonzero_exit_without_exposing_response_body() {
    let (url, server) = serve("401 Unauthorized", r#"{"error":"private detail"}"#);
    let output = Command::new(env!("CARGO_BIN_EXE_hermes-agent-rs"))
        .args(["--base-url", &url, "-m", "test-model", "-q", "hello"])
        .env_remove("OPENAI_API_KEY")
        .env("NO_PROXY", "127.0.0.1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("401"), "{error}");
    assert!(!error.contains("private detail"));
    server.join().unwrap();
}

#[test]
fn repl_saves_completed_turns_and_single_query_resumes_them() {
    let dir = tempfile::tempdir().unwrap();
    let history = dir.path().join("history.json");
    let (url, server) = serve(
        "200 OK",
        r#"{"choices":[{"message":{"content":"first answer"}}]}"#,
    );
    let mut cli = Command::new(env!("CARGO_BIN_EXE_hermes-agent-rs"))
        .args(["--base-url", &url, "-m", "test-model", "--history"])
        .arg(&history)
        .env_remove("OPENAI_API_KEY")
        .env("NO_PROXY", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    cli.stdin
        .take()
        .unwrap()
        .write_all(b"first query\n/exit\n")
        .unwrap();
    assert!(cli.wait_with_output().unwrap().status.success());
    server.join().unwrap();

    let (url, server) = serve(
        "200 OK",
        r#"{"choices":[{"message":{"content":"second answer"}}]}"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_hermes-agent-rs"))
        .args([
            "--base-url",
            &url,
            "-m",
            "test-model",
            "-q",
            "second query",
            "--history",
        ])
        .arg(&history)
        .env_remove("OPENAI_API_KEY")
        .env("NO_PROXY", "127.0.0.1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (_, body) = server.join().unwrap();
    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        request["messages"],
        serde_json::json!([
            {"role": "user", "content": "first query"},
            {"role": "assistant", "content": "first answer"},
            {"role": "user", "content": "second query"}
        ])
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(history).unwrap()).unwrap();
    assert_eq!(saved["messages"][3]["content"], "second answer");
}

#[test]
fn tool_result_reaches_the_provider_in_the_next_round() {
    use hermes_agent_rs::{Completions, Conversation, Tools};
    use serde_json::{Value, json};

    struct Lookup;
    impl Tools for Lookup {
        fn schemas(&self) -> Vec<Value> {
            vec![
                json!({"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}),
            ]
        }

        fn call(&mut self, name: &str, args: Value) -> Result<String, String> {
            assert_eq!(name, "lookup");
            assert_eq!(args, json!({"id":7}));
            Ok(r#"{"label":"seven"}"#.into())
        }
    }

    let (url, server) = serve_many(&[
        (
            "200 OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"id\":7}"}}]}}]}"#,
        ),
        ("200 OK", r#"{"choices":[{"message":{"content":"seven"}}]}"#),
    ]);
    let client = Completions::new(url.parse().unwrap(), None, "test-model".into()).unwrap();
    let mut conversation = Conversation::new(Vec::new(), Lookup);
    assert_eq!(conversation.turn(&client, "Find 7").unwrap(), "seven");
    let requests = server.join().unwrap();
    let first: Value = serde_json::from_str(&requests[0].1).unwrap();
    let second: Value = serde_json::from_str(&requests[1].1).unwrap();
    assert_eq!(first["tools"], second["tools"]);
    assert_eq!(first["messages"][0], second["messages"][0]);
    assert_eq!(
        second["messages"][2],
        json!({
            "role": "tool", "tool_call_id": "call-1", "content": r#"{"label":"seven"}"#
        })
    );
}

struct Proxy(std::process::Child);
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_proxy(url: &str) -> (Proxy, String) {
    let mut proxy = Proxy(
        Command::new(env!("CARGO_BIN_EXE_hermes-agent-rs"))
            .args([
                "--base-url",
                url,
                "-m",
                "test-model",
                "--gateway-listen",
                "127.0.0.1:0",
            ])
            .env_remove("OPENAI_API_KEY")
            .env("GATEWAY_PROXY_KEY", "proxy-test-key")
            .env("NO_PROXY", "127.0.0.1")
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(proxy.0.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let address = line
        .trim()
        .strip_prefix("Hermes Rust gateway proxy listening on ")
        .expect(&line);
    let address = address.to_owned();
    (proxy, address)
}

#[test]
fn gateway_proxy_requires_authentication_and_returns_the_existing_sse_shape() {
    use serde_json::{Value, json};

    let (url, upstream) = serve(
        "200 OK",
        r#"{"choices":[{"message":{"content":"gateway reply"}}]}"#,
    );
    let (_proxy, address) = start_proxy(&url);
    let endpoint = format!("http://{address}/v1/chat/completions");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let messages = json!([
        {"role":"system","content":"context"},
        {"role":"user","content":"previous"},
        {"role":"assistant","content":"previous reply"},
        {"role":"user","content":"new question"}
    ]);
    let body = json!({"model":"hermes-agent","messages":messages,"stream":true});
    assert_eq!(
        client.post(&endpoint).json(&body).send().unwrap().status(),
        401
    );
    let response = client
        .post(endpoint)
        .bearer_auth("proxy-test-key")
        .header("X-Hermes-Session-Id", "fixture")
        .json(&body)
        .send()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(
        response.text().unwrap(),
        "data: {\"choices\":[{\"delta\":{\"content\":\"gateway reply\"}}]}\n\ndata: [DONE]\n\n"
    );
    let (_, request) = upstream.join().unwrap();
    let request: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request, json!({"model":"test-model","messages":messages}));
}

#[test]
#[ignore = "requires a Python environment that can import Hermes SessionDB"]
fn resumes_history_created_by_python_session_db() {
    use serde_json::{Value, json};

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let fixture = r#"
from pathlib import Path
from hermes_state import SessionDB
import sys
db = SessionDB(Path(sys.argv[1]))
db.create_session("fixture", "cli")
db.update_system_prompt("fixture", "stored system prompt")
db.append_message("fixture", "user", "display text", api_content="provider text")
db.append_message("fixture", "assistant", "prior answer")
db.close()
"#;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("HERMES_PYTHON").unwrap_or_else(|| "python".into());
    let output = Command::new(python)
        .current_dir(root)
        .env("HERMES_HOME", dir.path())
        .env("HERMES_RUNTIME_DIR", dir.path())
        .args(["-c", fixture])
        .arg(&db)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes_before = std::fs::read(&db).unwrap();

    let (url, upstream) = serve(
        "200 OK",
        r#"{"choices":[{"message":{"content":"continued"}}]}"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_hermes-agent-rs"))
        .args([
            "--base-url",
            &url,
            "-m",
            "test-model",
            "--resume-session",
            "fixture",
            "-q",
            "next",
            "--state-db",
        ])
        .arg(&db)
        .env_remove("OPENAI_API_KEY")
        .env("NO_PROXY", "127.0.0.1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(db).unwrap(), bytes_before);
    let (_, request) = upstream.join().unwrap();
    let request: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(
        request["messages"],
        json!([
            {"role":"system","content":"stored system prompt"},
            {"role":"user","content":"provider text"},
            {"role":"assistant","content":"prior answer"},
            {"role":"user","content":"next"}
        ])
    );
}

#[test]
fn builtin_todo_requires_cli_opt_in() {
    use serde_json::{Value, json};

    const CALL: &str = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"todo-1","type":"function","function":{"name":"todo_list","arguments":"{\"todos\":[{\"id\":\"t1\",\"content\":\"Verify\",\"status\":\"in_progress\"}]}"}}]}}]}"#;
    const ANSWER: &str = r#"{"choices":[{"message":{"content":"done"}}]}"#;

    for allow in [false, true] {
        let (url, upstream) = serve_many(&[("200 OK", CALL), ("200 OK", ANSWER)]);
        let mut command = Command::new(env!("CARGO_BIN_EXE_hermes-agent-rs"));
        command
            .args(["--base-url", &url, "-m", "test-model", "-q", "plan"])
            .env_remove("OPENAI_API_KEY")
            .env("NO_PROXY", "127.0.0.1");
        if allow {
            command.arg("--allow-todo");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = upstream.join().unwrap();
        let first: Value = serde_json::from_str(&requests[0].1).unwrap();
        let second: Value = serde_json::from_str(&requests[1].1).unwrap();
        assert_eq!(first.get("tools").is_some(), allow);
        let result: Value =
            serde_json::from_str(second["messages"][2]["content"].as_str().unwrap()).unwrap();
        if allow {
            assert_eq!(
                result,
                json!({
                    "todos":[{"id":"t1","content":"Verify","status":"in_progress"}],
                    "revision":1,"summary":{"total":1,"pending":0,"in_progress":1,"completed":0,"cancelled":0}
                })
            );
        } else {
            assert_eq!(result, json!({"error":"tool is not enabled: todo_list"}));
        }
    }
}

#[test]
#[ignore = "requires a Python environment that can import Hermes GatewayRunner"]
fn python_gateway_runner_consumes_rust_proxy_reply() {
    let (url, upstream) = serve(
        "200 OK",
        r#"{"choices":[{"message":{"content":"Rust からの応答"}}]}"#,
    );
    let (_proxy, address) = start_proxy(&url);
    let dir = tempfile::tempdir().unwrap();
    let fixture = r#"
import asyncio
from gateway.config import Platform
from gateway.run import GatewayRunner
from gateway.session import SessionSource

class HeadlessRunner(GatewayRunner):
    def _thread_metadata_for_source(self, *args):
        return None

    def _delivery_adapter_for(self, *args):
        return None

    def _proxy_stream_consumer(self, *args):
        return None

runner = object.__new__(HeadlessRunner)
source = SessionSource(platform=Platform.LOCAL, chat_id="fixture")
result = asyncio.run(runner._run_agent_via_proxy(
    "new question", "context", [{"role": "user", "content": "previous"}], source, "fixture"
))
assert result["final_response"] == "Rust からの応答", result
assert result["history_offset"] == 1, result
assert result["session_id"] == "fixture", result
"#;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("HERMES_PYTHON").unwrap_or_else(|| "python".into());
    let output = Command::new(python)
        .current_dir(root)
        .env("HERMES_HOME", dir.path())
        .env("HERMES_RUNTIME_DIR", dir.path())
        .env("GATEWAY_PROXY_URL", format!("http://{address}"))
        .env("GATEWAY_PROXY_KEY", "proxy-test-key")
        .args(["-c", fixture])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (_, request) = upstream.join().unwrap();
    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request["messages"][0]["content"], "context");
    assert_eq!(request["messages"][1]["content"], "previous");
    assert_eq!(request["messages"][2]["content"], "new question");
}
