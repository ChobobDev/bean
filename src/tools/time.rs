use chrono::Local;

use super::{Args, BeanTool, ToolContext, ToolResult};

/// The model has no clock, so it needs this for anything date-related.
pub struct CurrentTime;

impl BeanTool for CurrentTime {
    fn name(&self) -> &'static str {
        "getCurrentTime"
    }

    fn description(&self) -> &'static str {
        "Returns the current date, time, and time zone. Use it whenever the answer depends on \
         today's date or the current time, such as 'what day is it' or 'how long until Friday'."
    }

    fn run(&self, _args: &Args, ctx: &ToolContext) -> ToolResult {
        ctx.record("Checked the time");
        Ok(Local::now()
            .format("%A, %-d %B %Y, %H:%M (UTC%:z)")
            .to_string())
    }
}
