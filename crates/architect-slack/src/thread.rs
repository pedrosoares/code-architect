//! Read a thread: the parent message and every reply.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::SlackClient;
use crate::format::format_message;

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    link: Option<String>,
}

pub struct ReadThread {
    pub client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for ReadThread {
    fn name(&self) -> &'static str {
        "slack_read_thread"
    }

    fn description(&self) -> &'static str {
        "Read a Slack thread: the parent message and every reply, in order. Provide either a \
         pasted Slack message/thread link, or a channel ID plus the thread's timestamp \
         (discoverable via slack_list_channels and slack_read_channel_history)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "link": {
                    "type": "string",
                    "description": "A Slack message or thread permalink, e.g. https://workspace.slack.com/archives/C0123456789/p1700000000000100 — an alternative to channel + thread_ts.",
                },
                "channel": {"type": "string", "description": "Channel ID, e.g. C0123456789"},
                "thread_ts": {
                    "type": "string",
                    "description": "Timestamp of the thread's parent message, e.g. 1234567890.123456",
                },
            },
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let (channel, thread_ts) = match &input.link {
            Some(link) => parse_slack_link(link)?,
            None => (
                input.channel.clone().ok_or_else(missing_target)?,
                input.thread_ts.clone().ok_or_else(missing_target)?,
            ),
        };

        let mut messages: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut params = vec![("channel", channel.as_str()), ("ts", thread_ts.as_str())];
            if let Some(cursor) = &cursor {
                params.push(("cursor", cursor.as_str()));
            }

            let body = self.client.call("conversations.replies", &params).await?;
            messages.extend(body["messages"].as_array().cloned().unwrap_or_default());

            cursor = body["response_metadata"]["next_cursor"]
                .as_str()
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }

        if messages.is_empty() {
            return Ok("No messages found in this thread.".to_owned().into());
        }

        Ok(messages
            .iter()
            .map(format_message)
            .collect::<Vec<_>>()
            .join("\n\n")
            .into())
    }
}

fn missing_target() -> String {
    "either \"link\", or both \"channel\" and \"thread_ts\", are required".to_owned()
}

/// Parses a Slack permalink like
/// `https://workspace.slack.com/archives/C0123456789/p1700000000000100` (a
/// link to a top-level message) or the same with `?thread_ts=...&cid=...`
/// appended (a link to a reply inside a thread) into `(channel, thread_ts)`.
fn parse_slack_link(link: &str) -> Result<(String, String), String> {
    let url = reqwest::Url::parse(link)
        .map_err(|_| format!("\"{link}\" doesn't look like a Slack link"))?;
    let segments: Vec<&str> = url
        .path_segments()
        .map(Iterator::collect)
        .unwrap_or_default();
    let [.., channel, p_segment] = segments.as_slice() else {
        return Err(format!(
            "could not find a channel and message in \"{link}\" — expected something like \
             https://workspace.slack.com/archives/C0123456789/p1700000000000100"
        ));
    };

    let digits = p_segment
        .strip_prefix('p')
        .filter(|digits| digits.len() > 10 && digits.bytes().all(|b| b.is_ascii_digit()));
    let Some(digits) = digits else {
        return Err(format!(
            "could not parse a timestamp out of \"{p_segment}\""
        ));
    };

    // A link to a reply carries the thread's real parent in `thread_ts`; a
    // link to a top-level message has no such param, and the message's own
    // timestamp (from the `p...` segment) doubles as the thread ts —
    // `conversations.replies` on a non-thread message's own ts just returns
    // that single message, which is a reasonable fallback.
    let thread_ts = url
        .query_pairs()
        .find(|(key, _)| key == "thread_ts")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| {
            let (secs, micros) = digits.split_at(10);
            format!("{secs}.{micros}")
        });

    Ok((channel.to_string(), thread_ts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_link_to_a_top_level_message() {
        let link = "https://workspace.slack.com/archives/C0123456789/p1700000000000100";
        assert_eq!(
            parse_slack_link(link).unwrap(),
            ("C0123456789".to_owned(), "1700000000.000100".to_owned())
        );
    }

    #[test]
    fn parses_a_link_to_a_reply_using_its_thread_ts_query_param() {
        let link = "https://workspace.slack.com/archives/C0123456789/p1700000000000200?thread_ts=1700000000.000100&cid=C0123456789";
        assert_eq!(
            parse_slack_link(link).unwrap(),
            ("C0123456789".to_owned(), "1700000000.000100".to_owned())
        );
    }

    #[test]
    fn a_malformed_link_is_a_clear_error() {
        let error = parse_slack_link("https://example.com/not-slack").unwrap_err();
        assert!(error.contains("could not"), "got: {error}");
    }

    #[test]
    fn a_non_url_is_a_clear_error() {
        let error = parse_slack_link("not a url at all").unwrap_err();
        assert!(
            error.contains("doesn't look like a Slack link"),
            "got: {error}"
        );
    }
}
