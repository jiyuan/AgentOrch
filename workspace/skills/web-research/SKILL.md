---
name: web-research
description: Fetch web pages through approved AgentOS HTTP tool calls and summarize the fetched result. Use when a user asks for web research, a URL fetch, or the top Hacker News story.
---

# Web Research

Use the AgentOS run loop for every external fetch. The skill plans an `http` tool call, lets approval and guardrails run in the normal loop, then summarizes only after the tool result is appended to the transcript.

## Execution Workflow

1. Match user requests that start with `web research:` or `web_research:`.
2. Match requests that ask to summarize the top story on Hacker News.
3. Plan a structured `http` GET tool call.
4. Wait for the tool result to return through `Act -> Observe -> Plan`.
5. Summarize the first Hacker News title when available, otherwise summarize the HTML title or a short text excerpt.

## Validation

Run `agentos skill validate web-research` after editing this skill. The validator checks that this file keeps Anthropic-compatible `SKILL.md` frontmatter, including a hyphen-case `name`, a non-empty `description`, and Markdown body instructions.
