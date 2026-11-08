//! Turning Discord messages into prompts.

/// Strips mentions of the bot (as a user or via one of its roles) from
/// `content` and trims the result.
pub fn clean_prompt(content: &str, bot_id: u64, bot_roles: &[u64]) -> String {
    let mut text = content
        .replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "");
    for role in bot_roles {
        text = text.replace(&format!("<@&{role}>"), "");
    }
    text.trim().to_string()
}

/// Builds the prompt sent to the model. In shared channels the speaker's
/// name is included so the model can tell people apart.
pub fn build_prompt(author: &str, text: &str, shared_channel: bool) -> String {
    if shared_channel {
        format!("{author}: {text}")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_user_and_nickname_mentions() {
        assert_eq!(clean_prompt("<@42> hi there", 42, &[]), "hi there");
        assert_eq!(clean_prompt("hey <@!42>", 42, &[]), "hey");
    }

    #[test]
    fn strips_bot_role_mentions_only() {
        let text = clean_prompt("<@&7> ask <@&8> about it", 42, &[7]);
        assert_eq!(text, "ask <@&8> about it");
    }

    #[test]
    fn keeps_other_users_mentions() {
        assert_eq!(
            clean_prompt("<@42> say hi to <@99>", 42, &[]),
            "say hi to <@99>"
        );
    }

    #[test]
    fn mention_only_message_is_empty() {
        assert_eq!(clean_prompt("  <@42>  ", 42, &[]), "");
    }

    #[test]
    fn names_speaker_in_shared_channels() {
        assert_eq!(build_prompt("ada", "hello", true), "ada: hello");
        assert_eq!(build_prompt("ada", "hello", false), "hello");
    }
}
