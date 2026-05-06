use agentos_interfaces::guardrail::{
    GuardrailError, GuardrailOutcome, Input, InputGuardrail, OutputGuardrail, ToolGuardrail,
};
use agentos_interfaces::orchestrator::RunContext;
use agentos_proto::{Message, ToolCall, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::Arc;

pub struct PiiFilter;

#[async_trait]
impl InputGuardrail for PiiFilter {
    async fn check(
        &self,
        input: &Input,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        if contains_email_like(&input.message.content) {
            return Ok(GuardrailOutcome::Tripped(Arc::from(
                "input appears to contain an email address",
            )));
        }
        if contains_ssn_like(&input.message.content) {
            return Ok(GuardrailOutcome::Tripped(Arc::from(
                "input appears to contain a US social security number",
            )));
        }
        Ok(GuardrailOutcome::Passed)
    }
}

pub struct MaxOutputLength {
    max_chars: usize,
}

impl MaxOutputLength {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

#[async_trait]
impl OutputGuardrail for MaxOutputLength {
    async fn check(
        &self,
        output: &Message,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        let chars = output.content.chars().count();
        if chars > self.max_chars {
            return Ok(GuardrailOutcome::Tripped(Arc::from(format!(
                "output has {chars} characters, limit is {}",
                self.max_chars
            ))));
        }
        Ok(GuardrailOutcome::Passed)
    }
}

pub struct ShellCommandAllowlist {
    allowed: BTreeSet<Arc<str>>,
}

impl ShellCommandAllowlist {
    pub fn new(commands: impl IntoIterator<Item = impl Into<Arc<str>>>) -> Self {
        Self {
            allowed: commands.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShellCallArgs {
    command: String,
}

#[async_trait]
impl ToolGuardrail for ShellCommandAllowlist {
    async fn check_call(
        &self,
        call: &ToolCall,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        if call.name.as_ref() != "shell" {
            return Ok(GuardrailOutcome::Passed);
        }

        let parsed: ShellCallArgs = serde_json::from_str(call.args.get())
            .map_err(|err| GuardrailError::Backend(err.to_string().into()))?;
        if self.allowed.contains(parsed.command.as_str()) {
            Ok(GuardrailOutcome::Passed)
        } else {
            Ok(GuardrailOutcome::Tripped(Arc::from(format!(
                "shell command '{}' is not allowlisted",
                parsed.command
            ))))
        }
    }

    async fn check_result(
        &self,
        _result: &ToolResult,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        Ok(GuardrailOutcome::Passed)
    }
}

fn contains_email_like(input: &str) -> bool {
    input.split_whitespace().any(|token| {
        let Some((local, domain)) = token.split_once('@') else {
            return false;
        };
        !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
    })
}

fn contains_ssn_like(input: &str) -> bool {
    input.as_bytes().windows(11).any(|window| {
        window[0].is_ascii_digit()
            && window[1].is_ascii_digit()
            && window[2].is_ascii_digit()
            && window[3] == b'-'
            && window[4].is_ascii_digit()
            && window[5].is_ascii_digit()
            && window[6] == b'-'
            && window[7].is_ascii_digit()
            && window[8].is_ascii_digit()
            && window[9].is_ascii_digit()
            && window[10].is_ascii_digit()
    })
}

