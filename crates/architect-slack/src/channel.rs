//! Discover channels, and browse a channel's recent activity — the two
//! things needed to find a thread worth reading with `slack_read_thread`
//! in the first place.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::SlackClient;
use crate::format::format_message;

pub struct ListChannels {
    pub client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for ListChannels {
    fn name(&self) -> &'static str {
        "slack_list_channels"
    }

    fn description(&self) -> &'static str {
        "List Slack channels this token can see (public and private, excluding archived), with \
         each channel's ID, name, and privacy/membership status — use this to find a channel ID \
         before calling slack_read_channel_history or slack_read_thread."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    async fn call(&self, _input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let mut channels: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut params = vec![
                ("types", "public_channel,private_channel"),
                ("exclude_archived", "true"),
                ("limit", "200"),
            ];
            if let Some(cursor) = &cursor {
                params.push(("cursor", cursor.as_str()));
            }

            let body = self.client.call("conversations.list", &params).await?;
            channels.extend(body["channels"].as_array().cloned().unwrap_or_default());

            cursor = body["response_metadata"]["next_cursor"]
                .as_str()
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }

        if channels.is_empty() {
            return Ok("No channels found.".to_owned().into());
        }

        let mut lines: Vec<String> = channels.iter().map(format_channel).collect();
        lines.sort();
        Ok(lines.join("\n").into())
    }
}

fn format_channel(channel: &Value) -> String {
    let name = channel["name"].as_str().unwrap_or("?");
    let id = channel["id"].as_str().unwrap_or("?");
    let privacy = if channel["is_private"].as_bool().unwrap_or(false) {
        "private"
    } else {
        "public"
    };
    let member = if channel["is_member"].as_bool().unwrap_or(false) {
        "yes"
    } else {
        "no"
    };

    let mut line = format!("#{name}  id={id}  {privacy}  member={member}");

    let topic = channel["topic"]["value"].as_str().unwrap_or("");
    if !topic.is_empty() {
        line.push_str(&format!("\n    topic: {topic}"));
    }

    line
}

#[derive(Deserialize)]
struct HistoryInput {
    channel: String,
    #[serde(default)]
    limit: Option<u32>,
}

pub struct ReadChannelHistory {
    pub client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for ReadChannelHistory {
    fn name(&self) -> &'static str {
        "slack_read_channel_history"
    }

    fn description(&self) -> &'static str {
        "Read a Slack channel's most recent messages, newest activity last. Messages that \
         started a thread are marked with their reply count and thread_ts — pass that thread_ts \
         to slack_read_thread to read the full thread."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {"type": "string", "description": "Channel ID, e.g. C0123456789"},
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of recent messages to return (default 50, max 200).",
                },
            },
            "required": ["channel"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: HistoryInput =
            serde_json::from_value(input).map_err(|error| error.to_string())?;
        let limit = input.limit.unwrap_or(50).clamp(1, 200).to_string();

        let body = self
            .client
            .call(
                "conversations.history",
                &[
                    ("channel", input.channel.as_str()),
                    ("limit", limit.as_str()),
                ],
            )
            .await?;

        let mut messages = body["messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return Ok("No messages found in this channel.".to_owned().into());
        }

        // Slack returns history newest-first; reverse for chronological
        // reading, matching slack_read_thread's oldest-first convention.
        messages.reverse();

        Ok(messages
            .iter()
            .map(format_history_message)
            .collect::<Vec<_>>()
            .join("\n\n")
            .into())
    }
}

fn format_history_message(message: &Value) -> String {
    let base = format_message(message);
    let reply_count = message["reply_count"].as_u64().unwrap_or(0);

    if reply_count == 0 {
        return base;
    }

    let thread_ts = message["ts"].as_str().unwrap_or_default();
    format!("{base} ({reply_count} replies, thread_ts={thread_ts})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_public_channel_with_no_topic() {
        let channel = json!({
            "id": "C1", "name": "general", "is_private": false, "is_member": true,
            "topic": {"value": ""},
        });

        assert_eq!(
            format_channel(&channel),
            "#general  id=C1  public  member=yes"
        );
    }

    #[test]
    fn formats_a_private_channel_with_a_topic() {
        let channel = json!({
            "id": "C2", "name": "incidents", "is_private": true, "is_member": false,
            "topic": {"value": "prod fires only"},
        });

        let text = format_channel(&channel);
        assert!(text.starts_with("#incidents  id=C2  private  member=no"));
        assert!(text.contains("topic: prod fires only"));
    }

    #[test]
    fn a_message_with_no_thread_has_no_reply_marker() {
        let message =
            json!({"user": "U1", "ts": "1700000000.000000", "text": "hi", "reply_count": 0});
        assert_eq!(
            format_history_message(&message),
            "[1700000000.000000] U1: hi"
        );
    }

    #[test]
    fn a_message_that_started_a_thread_shows_its_reply_count_and_ts() {
        let message = json!({
            "user": "U1", "ts": "1700000000.000000", "text": "shipping soon",
            "reply_count": 3,
        });

        let text = format_history_message(&message);
        assert!(
            text.contains("(3 replies, thread_ts=1700000000.000000)"),
            "got: {text}"
        );
    }
}
