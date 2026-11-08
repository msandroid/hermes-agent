mod completions;

pub use completions::Completions;

use serde_json::{Value, json};
use std::collections::HashSet;

/// Supplies a stable set of Chat Completions tool schemas and dispatches enabled tools.
/// Hosts own approval, profile scope and any external side effects.
pub trait Tools {
    fn schemas(&self) -> Vec<Value>;
    fn call(&mut self, name: &str, arguments: Value) -> Result<String, String>;
}

pub struct NoTools;

impl Tools for NoTools {
    fn schemas(&self) -> Vec<Value> {
        Vec::new()
    }

    fn call(&mut self, _name: &str, _arguments: Value) -> Result<String, String> {
        Err("tools are disabled".into())
    }
}

pub struct Conversation<T> {
    messages: Vec<Value>,
    schemas: Vec<Value>,
    enabled_tools: HashSet<String>,
    tools: T,
}

impl<T: Tools> Conversation<T> {
    pub fn new(messages: Vec<Value>, tools: T) -> Self {
        let schemas = tools.schemas();
        let enabled_tools = schemas
            .iter()
            .filter_map(|schema| {
                schema
                    .pointer("/function/name")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        Self {
            messages,
            schemas,
            enabled_tools,
            tools,
        }
    }

    pub fn messages(&self) -> &[Value] {
        &self.messages
    }

    pub fn turn(&mut self, client: &Completions, prompt: &str) -> Result<String, String> {
        let checkpoint = self.messages.len();
        let result = self.run_turn(client, prompt);
        if result.is_err() {
            self.messages.truncate(checkpoint);
        }
        result
    }

    fn run_turn(&mut self, client: &Completions, prompt: &str) -> Result<String, String> {
        self.messages
            .push(json!({"role": "user", "content": prompt}));
        let mut seen_ids = HashSet::new();
        for round in 0..=16 {
            let mut reply = client.request(&self.messages, &self.schemas)?;
            if reply.tool_calls.is_empty() {
                let answer = reply
                    .content
                    .take()
                    .filter(|text| !text.trim().is_empty())
                    .ok_or("provider returned no text answer")?;
                self.messages
                    .push(json!({"role": "assistant", "content": answer}));
                return Ok(answer);
            }
            if round == 16 {
                return Err("tool round limit reached".into());
            }
            if reply
                .tool_calls
                .iter()
                .any(|call| call.id.is_empty() || !seen_ids.insert(call.id.clone()))
            {
                return Err("provider returned an empty or duplicate tool call ID".into());
            }
            self.messages.push(json!({
                "role": "assistant", "content": reply.content, "tool_calls": reply.tool_calls
            }));
            for call in reply.tool_calls {
                let name = &call.function.name;
                let result = match serde_json::from_str::<Value>(&call.function.arguments) {
                    _ if call.kind != "function" || !self.enabled_tools.contains(name) => {
                        Err(format!("tool is not enabled: {name}"))
                    }
                    Ok(args) if args.is_object() => self.tools.call(name, args),
                    _ => Err("tool arguments must be a JSON object".into()),
                };
                let content = result.unwrap_or_else(|error| json!({"error": error}).to_string());
                self.messages
                    .push(json!({"role": "tool", "tool_call_id": call.id, "content": content}));
            }
        }
        unreachable!("the final iteration returns an answer or a limit error")
    }
}
