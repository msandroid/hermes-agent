use std::collections::{HashMap, HashSet};

use hermes_agent_rs::Tools;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "cancelled"];

#[derive(Clone, PartialEq, Deserialize, Serialize)]
struct Item {
    id: String,
    content: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}

#[derive(Default, Deserialize)]
pub struct Todo {
    #[serde(rename = "todos")]
    items: Vec<Item>,
    revision: u64,
}

impl Todo {
    pub fn restore(messages: &[Value]) -> Self {
        for (index, result) in messages.iter().enumerate().rev() {
            if result["role"] != "tool" {
                continue;
            }
            let Some(owner) = messages[..index]
                .iter()
                .rev()
                .find(|msg| msg["role"] != "tool")
                .filter(|msg| msg["role"] == "assistant")
            else {
                continue;
            };
            let Some(calls) = owner["tool_calls"].as_array() else {
                continue;
            };
            if calls.iter().any(|call| {
                call["id"] == result["tool_call_id"] && call["function"]["name"] == "todo_list"
            }) && let Some(content) = result["content"].as_str()
                && content.len() <= 512_000
                && let Ok(store) = serde_json::from_str::<Self>(content)
                && store.items.len() <= 256
            {
                return store;
            }
        }
        Self::default()
    }

    fn write(&mut self, input: &[Value], merge: bool) -> Result<(), String> {
        let mut items = if merge {
            self.items.clone()
        } else {
            Vec::new()
        };
        let mut seen = HashSet::new();
        let unique: Vec<_> = input
            .iter()
            .enumerate()
            .rev()
            .filter(|(index, item)| {
                let key = if item.is_object() {
                    id(item).to_owned()
                } else {
                    format!("__invalid_{index}")
                };
                seen.insert(key)
            })
            .collect();
        for (_, patch) in unique.into_iter().rev() {
            if merge && field(patch, "id").is_empty() {
                continue;
            }
            if merge && let Some(item) = items.iter_mut().find(|item| item.id == id(patch)) {
                if let Some(content) = patch["content"]
                    .as_str()
                    .filter(|content| !content.is_empty())
                {
                    item.content = capped(content.trim());
                }
                if let Some(status) = status(patch) {
                    item.status = status;
                }
                if patch.get("parent").is_some() {
                    item.parent = parent(patch);
                }
                continue;
            }
            let content = if patch.is_object() {
                field(patch, "content")
            } else {
                "(invalid item)"
            };
            if content.is_empty() {
                return Err(format!(
                    "Todo item '{}' has empty or missing content. Each item must have a non-empty description.",
                    id(patch)
                ));
            }
            items.push(Item {
                id: id(patch).into(),
                content: capped(content),
                status: status(patch).unwrap_or_else(|| "pending".into()),
                parent: parent(patch),
            });
        }
        if items.iter().all(|item| item.parent.is_none())
            && let Some(active) = items.iter().position(|item| item.status == "in_progress")
            && let Some(pending) = items[..active]
                .iter()
                .position(|item| item.status == "pending")
        {
            let item = items.remove(active);
            items.insert(pending, item);
        }
        items.truncate(256);
        sanitize_parents(&mut items);
        if items != self.items {
            self.items = items;
            self.revision = self.revision.saturating_add(1);
        }
        Ok(())
    }
}

impl Tools for Todo {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({"type":"function", "function":{
            "name":"todo_list", "description":"Track tasks in priority order. Omit todos to read the current list.",
            "parameters":{"type":"object","properties":{
                "todos":{"type":"array","items":{"type":"object","properties":{
                    "id":{"type":"string"},"content":{"type":"string"},"status":{"type":"string","enum":STATUSES},
                    "parent":{"type":"string"}
                },"required":["id","content","status"]}},
                "merge":{"type":"boolean","default":false}
            }}
        }})]
    }

    fn call(&mut self, name: &str, args: Value) -> Result<String, String> {
        if name != "todo_list" {
            return Err("tool is not enabled".into());
        }
        if !args["todos"].is_null() {
            let items = args["todos"].as_array().ok_or("todos must be a list")?;
            self.write(items, args["merge"].as_bool().unwrap_or(false))?;
        }
        let mut summary = json!({"total": self.items.len()});
        for status in STATUSES {
            summary[status] = json!(
                self.items
                    .iter()
                    .filter(|item| item.status == status)
                    .count()
            );
        }
        Ok(json!({"todos":self.items,"revision":self.revision,"summary":summary}).to_string())
    }
}

fn field<'a>(item: &'a Value, key: &str) -> &'a str {
    item[key].as_str().unwrap_or("").trim()
}

fn id(item: &Value) -> &str {
    let id = field(item, "id");
    if id.is_empty() { "?" } else { id }
}

fn parent(item: &Value) -> Option<String> {
    let parent = field(item, "parent");
    (!parent.is_empty() && parent != id(item)).then(|| parent.to_owned())
}

fn status(item: &Value) -> Option<String> {
    let status = field(item, "status").to_lowercase();
    STATUSES.contains(&status.as_str()).then_some(status)
}

fn capped(content: &str) -> String {
    const MARKER: &str = "… [truncated]";
    if content.chars().count() <= 4000 {
        return content.to_owned();
    }
    let mut prefix: String = content
        .chars()
        .take(4000 - MARKER.chars().count())
        .collect();
    prefix.push_str(MARKER);
    prefix
}

fn sanitize_parents(items: &mut [Item]) {
    let ids: HashSet<_> = items.iter().map(|item| item.id.clone()).collect();
    for item in items.iter_mut() {
        if item
            .parent
            .as_ref()
            .is_some_and(|parent| !ids.contains(parent))
        {
            item.parent = None;
        }
    }
    let mut parents: HashMap<_, _> = items
        .iter()
        .map(|item| (item.id.clone(), item.parent.clone()))
        .collect();
    for item in items {
        let mut seen = HashSet::from([item.id.as_str()]);
        let mut next = item.parent.as_deref();
        while let Some(parent) = next {
            if !seen.insert(parent) {
                item.parent = None;
                parents.insert(item.id.clone(), None);
                break;
            }
            next = parents.get(parent).and_then(|parent| parent.as_deref());
        }
    }
}
