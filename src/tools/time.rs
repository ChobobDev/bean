use chrono::Local;
use fm_rs::{Tool, ToolOutput};
use serde_json::{Value, json};

use super::{Shared, output};

/// The model has no clock, so it needs this for anything date-related.
pub struct CurrentTime {
    shared: Shared,
}

impl CurrentTime {
    pub(super) fn new(shared: Shared) -> Self {
        Self { shared }
    }
}

impl Tool for CurrentTime {
    fn name(&self) -> &str {
        "getCurrentTime"
    }

    fn description(&self) -> &str {
        "Returns the current date, time, and time zone. Use it whenever the answer depends on \
         today's date or the current time, such as 'what day is it' or 'how long until Friday'."
    }

    fn arguments_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn call(&self, _arguments: Value) -> fm_rs::Result<ToolOutput> {
        self.shared.record("Checked the time");
        output(Local::now().format("%A, %-d %B %Y, %H:%M (UTC%:z)").to_string())
    }
}
