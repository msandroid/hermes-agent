use std::time::Duration;

use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: &'a [Value],
    #[serde(skip_serializing_if = "<[Value]>::is_empty")]
    tools: &'a [Value],
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Answer,
}

#[derive(Deserialize)]
pub(crate) struct Answer {
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: Function,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Function {
    pub name: String,
    pub arguments: String,
}

pub struct Completions {
    endpoint: Url,
    client: Client,
    api_key: Option<String>,
    model: String,
}

impl Completions {
    pub fn new(mut endpoint: Url, api_key: Option<String>, model: String) -> Result<Self, String> {
        if !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(
                "base URL must be an HTTP(S) API root without credentials, query or fragment"
                    .into(),
            );
        }
        if endpoint.scheme() == "http" && api_key.is_some() && !is_loopback(&endpoint) {
            return Err("an API key requires HTTPS or a loopback endpoint".into());
        }
        endpoint
            .path_segments_mut()
            .map_err(|_| "invalid API root")?
            .pop_if_empty()
            .extend(["chat", "completions"]);

        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|err| format!("cannot initialize HTTP client: {}", err.without_url()))?;
        Ok(Self {
            endpoint,
            client,
            api_key,
            model,
        })
    }

    pub(crate) fn request(&self, messages: &[Value], tools: &[Value]) -> Result<Answer, String> {
        let mut request = self.client.post(self.endpoint.clone()).json(&Request {
            model: &self.model,
            messages,
            tools,
        });
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .map_err(|err| format!("provider request failed: {}", err.without_url()))?;
        if !response.status().is_success() {
            return Err(format!("provider returned HTTP {}", response.status()));
        }
        let response: Response = response
            .json()
            .map_err(|err| format!("invalid provider response: {}", err.without_url()))?;
        response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message)
            .ok_or_else(|| "provider returned no choices".into())
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}
