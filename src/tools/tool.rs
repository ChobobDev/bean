//! The [`BeanTool`] trait, and the adapter that exposes it to the model.

use std::fmt;
use std::sync::Arc;

use fm_rs::ToolOutput;
use serde_json::{Map, Value, json};
use tracing::debug;

use super::{Args, ToolContext};

/// Something the model can do while answering.
///
/// Tools are stateless; everything they need comes from the [`Args`] the
/// model passed and the [`ToolContext`] of the channel they're used in.
pub trait BeanTool: Send + Sync {
    /// The name the model calls the tool by. camelCase reads best to it.
    fn name(&self) -> &'static str;

    /// What the tool does and when to use it. This is the only guidance
    /// the model gets, so be explicit about when to call it.
    fn description(&self) -> &'static str;

    fn params(&self) -> &'static [Param] {
        &[]
    }

    /// Whether the tool works in `ctx`. Tools that need a Discord channel
    /// return false without one.
    fn available(&self, ctx: &ToolContext) -> bool {
        let _ = ctx;
        true
    }

    /// Runs the tool. Errors are shown to the model, which usually
    /// explains them to the user or retries with better arguments.
    fn run(&self, args: &Args, ctx: &ToolContext) -> ToolResult;
}

pub type ToolResult = Result<String, ToolError>;

/// A problem the model should hear about, e.g. "couldn't find that city".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(String);

impl ToolError {
    pub fn new(message: impl fmt::Display) -> Self {
        Self(message.to_string())
    }

    pub fn missing(param: &str) -> Self {
        Self(format!("missing the \"{param}\" argument"))
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One argument a tool accepts.
pub struct Param {
    pub name: &'static str,
    pub kind: ParamKind,
    pub description: &'static str,
}

#[derive(Clone, Copy)]
pub enum ParamKind {
    Text,
    Integer,
    Number,
}

impl Param {
    pub const fn text(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            kind: ParamKind::Text,
            description,
        }
    }

    pub const fn number(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            kind: ParamKind::Number,
            description,
        }
    }

    pub const fn integer(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            kind: ParamKind::Integer,
            description,
        }
    }
}

/// The JSON schema for `params`.
///
/// Nothing is marked `required`. The bridge rejects calls missing a
/// required argument before they reach Rust, and the on-device model often
/// mislabels arguments, so it would retry the same bad call over and over.
/// Tools check their own arguments instead, and [`Args`] recovers
/// mislabeled ones.
pub(super) fn schema(params: &[Param]) -> Value {
    let properties: Map<String, Value> = params
        .iter()
        .map(|param| {
            let kind = match param.kind {
                ParamKind::Text => "string",
                ParamKind::Integer => "integer",
                ParamKind::Number => "number",
            };
            let property = json!({ "type": kind, "description": param.description });
            (param.name.to_string(), property)
        })
        .collect();
    json!({ "type": "object", "properties": properties })
}

/// Exposes a [`BeanTool`] to the model, bound to one channel's context.
pub(super) struct Adapter {
    pub(super) tool: &'static dyn BeanTool,
    pub(super) ctx: Arc<ToolContext>,
}

impl fm_rs::Tool for Adapter {
    fn name(&self) -> &str {
        self.tool.name()
    }

    fn description(&self) -> &str {
        self.tool.description()
    }

    fn arguments_schema(&self) -> Value {
        schema(self.tool.params())
    }

    /// Never returns `Err`: that would abort the whole reply. Problems are
    /// reported to the model as the tool's output instead.
    fn call(&self, arguments: Value) -> fm_rs::Result<ToolOutput> {
        let name = self.tool.name();
        debug!(tool = name, %arguments, "tool call");
        let output = match self.tool.run(&Args::new(&arguments), &self.ctx) {
            Ok(output) => output,
            Err(e) => {
                debug!(tool = name, "tool error: {e}");
                format!("Error: {e}")
            }
        };
        Ok(ToolOutput::new(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_lists_params_without_requiring_any() {
        let params = [
            Param::text("city", "A city"),
            Param::integer("count", "How many"),
        ];
        assert_eq!(
            schema(&params),
            json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string", "description": "A city" },
                    "count": { "type": "integer", "description": "How many" }
                }
            })
        );
    }

    #[test]
    fn schema_for_no_params_is_an_empty_object() {
        assert_eq!(schema(&[]), json!({ "type": "object", "properties": {} }));
    }
}
