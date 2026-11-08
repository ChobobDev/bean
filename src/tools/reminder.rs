//! Setting and listing reminders. Delivery lives in [`crate::reminders`].

use chrono::{DateTime, Local};

use super::{Args, BeanTool, Param, ToolContext, ToolError, ToolResult};

pub struct SetReminder;

impl BeanTool for SetReminder {
    fn name(&self) -> &'static str {
        "setReminder"
    }

    fn description(&self) -> &'static str {
        "Schedules a reminder that pings the person you're talking to in this channel after a \
         delay. Use it whenever someone asks to be reminded of something. Always pass both \
         'delay' and 'message'."
    }

    fn params(&self) -> &'static [Param] {
        const PARAMS: &[Param] = &[
            Param::text(
                "delay",
                "How long from now, e.g. '20 minutes', '2 hours', or '1 day' (at most 7 days)",
            ),
            Param::text("message", "What to remind them about"),
        ];
        PARAMS
    }

    fn available(&self, ctx: &ToolContext) -> bool {
        ctx.discord().is_some()
    }

    fn run(&self, args: &Args, ctx: &ToolContext) -> ToolResult {
        let user_id = ctx.author_id()?;
        let channel_id = ctx.require_discord()?.channel_id.get();
        // Read the delay first so its text can't be mistaken for the message.
        let delay = args
            .duration(&["delay", "minutes", "time", "duration", "in"])
            .ok_or_else(|| ToolError::new("missing the \"delay\" argument, e.g. \"2 hours\""))?;
        let message = args.require_text("message")?;

        let reminder = ctx
            .reminders()
            .add(channel_id, user_id, delay, message)
            .map_err(ToolError::new)?;
        let due = format_due(reminder.due_at);
        ctx.record(format!("Set a reminder for {due}"));
        Ok(format!("Reminder set for {due}: {}", reminder.text))
    }
}

pub struct ListReminders;

impl BeanTool for ListReminders {
    fn name(&self) -> &'static str {
        "listReminders"
    }

    fn description(&self) -> &'static str {
        "Lists the reminders the person you're talking to has pending. Use it when they ask what \
         reminders they have."
    }

    fn available(&self, ctx: &ToolContext) -> bool {
        ctx.discord().is_some()
    }

    fn run(&self, _args: &Args, ctx: &ToolContext) -> ToolResult {
        let user_id = ctx.author_id()?;
        ctx.record("Checked reminders");
        let pending = ctx.reminders().pending_for(user_id);
        if pending.is_empty() {
            return Ok("No reminders pending.".into());
        }
        let lines: Vec<_> = pending
            .iter()
            .map(|r| format!("- {}: {}", format_due(r.due_at), r.text))
            .collect();
        Ok(lines.join("\n"))
    }
}

fn format_due(unix_secs: u64) -> String {
    DateTime::from_timestamp(unix_secs as i64, 0)
        .map(|t| {
            t.with_timezone(&Local)
                .format("%a %-d %b, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "an unknown time".into())
}
