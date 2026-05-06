use super::WorkspaceSkillCatalog;
use agentos_interfaces::orchestrator::{OrchestratorError, Plan, RunContext};
use agentos_proto::{Message, MessageRole, ToolCall, ToolCallId};
use serde_json::{json, value::RawValue};
use std::sync::Arc;

pub struct WebResearchSkill<'a> {
    catalog: &'a WorkspaceSkillCatalog,
}

impl<'a> WebResearchSkill<'a> {
    pub fn new(catalog: &'a WorkspaceSkillCatalog) -> Self {
        Self { catalog }
    }

    pub fn plan(&self, ctx: &RunContext<'_>) -> Result<Option<Plan>, OrchestratorError> {
        if !self.catalog.contains("web-research") {
            return Ok(None);
        }
        let Some(item) = ctx.state.transcript.items.last() else {
            return Ok(None);
        };

        match item.message.role {
            MessageRole::User => self.plan_fetch(&item.message.content),
            MessageRole::Tool => self.plan_summary(ctx),
            MessageRole::Assistant | MessageRole::System => Ok(None),
        }
    }

    fn plan_fetch(&self, input: &str) -> Result<Option<Plan>, OrchestratorError> {
        let lower = input.to_ascii_lowercase();
        let url = if lower.contains("summarize")
            && lower.contains("top")
            && lower.contains("hacker news")
        {
            "https://news.ycombinator.com/news"
        } else if let Some(url) = input.strip_prefix("web research:") {
            url.trim()
        } else if let Some(url) = input.strip_prefix("web_research:") {
            url.trim()
        } else {
            return Ok(None);
        };

        Ok(Some(raw_tool_plan(
            "http",
            "web-research-fetch-1",
            json!({
                "method": "GET",
                "url": url
            }),
        )?))
    }

    fn plan_summary(&self, ctx: &RunContext<'_>) -> Result<Option<Plan>, OrchestratorError> {
        if !previous_user_requested_web_research(ctx) {
            return Ok(None);
        }

        let Some(item) = ctx.state.transcript.items.last() else {
            return Ok(None);
        };
        let summary = summarize_research_result(&item.message.content);
        Ok(Some(Plan::Reply(Message::text(
            MessageRole::Assistant,
            summary,
        ))))
    }
}

fn raw_tool_plan(
    name: &str,
    call_id: &str,
    args: serde_json::Value,
) -> Result<Plan, OrchestratorError> {
    let raw_args = RawValue::from_string(args.to_string())
        .map_err(|err| OrchestratorError::Backend(err.to_string().into()))?;
    Ok(Plan::CallTool(ToolCall {
        id: ToolCallId::new(call_id),
        name: Arc::from(name),
        args: raw_args,
    }))
}

fn previous_user_requested_web_research(ctx: &RunContext<'_>) -> bool {
    ctx.state.transcript.items.iter().rev().skip(1).any(|item| {
        if item.message.role != MessageRole::User {
            return false;
        }
        let lower = item.message.content.to_ascii_lowercase();
        lower.starts_with("web research:")
            || lower.starts_with("web_research:")
            || (lower.contains("summarize")
                && lower.contains("top")
                && lower.contains("hacker news"))
    })
}

fn summarize_research_result(content: &str) -> String {
    if let Some((title, url)) = first_hacker_news_story(content) {
        return format!("Top Hacker News story: {title}\nSource: {url}");
    }

    if let Some(title) = html_title(content) {
        return format!("Fetched page: {title}");
    }

    let plain = collapse_whitespace(strip_tags(content));
    if plain.is_empty() {
        "Fetched the page, but no readable text was found.".to_owned()
    } else {
        let excerpt = plain.chars().take(240).collect::<String>();
        format!("Fetched page excerpt: {excerpt}")
    }
}

fn first_hacker_news_story(content: &str) -> Option<(String, String)> {
    let marker = "class=\"titleline\"";
    let start = content.find(marker)?;
    let after_marker = &content[start..];
    let href_start = after_marker.find("href=\"")? + "href=\"".len();
    let after_href = &after_marker[href_start..];
    let href_end = after_href.find('"')?;
    let href = &after_href[..href_end];
    let anchor_close = after_href.find('>')? + 1;
    let after_anchor = &after_href[anchor_close..];
    let title_end = after_anchor.find("</a>")?;
    let title = decode_html_entities(&strip_tags(&after_anchor[..title_end]));
    if title.trim().is_empty() {
        return None;
    }
    Some((collapse_whitespace(title), href.to_owned()))
}

fn html_title(content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = lower[start..].find("</title>")? + start;
    let title = decode_html_entities(&content[start..end]);
    let title = collapse_whitespace(title);
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

fn strip_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

fn collapse_whitespace(input: impl AsRef<str>) -> String {
    input
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

