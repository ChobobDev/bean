//! Explaining failures to the people talking to Bean.

use fm_rs::Error as FmError;

/// A short, human-readable explanation of a model error.
pub fn user_facing_error(error: &FmError) -> String {
    match error {
        FmError::AppleIntelligenceNotEnabled => {
            "Apple Intelligence is turned off on the Mac running me.".into()
        }
        FmError::DeviceNotEligible | FmError::ModelNotAvailable => {
            "The Mac running me can't use Apple Intelligence.".into()
        }
        FmError::ModelNotReady | FmError::AssetsUnavailable(_) => {
            "My model is still getting ready. Try again in a minute.".into()
        }
        FmError::GuardrailViolation(_) => "Apple's safety filter blocked that one.".into(),
        FmError::Refusal(_) => "I can't help with that.".into(),
        FmError::ContextSizeExceeded(_) => "That message is too long for me.".into(),
        FmError::UnsupportedLanguageOrLocale(_) => "I can't respond in that language yet.".into(),
        FmError::RateLimited(_) => "I'm overloaded right now. Try again shortly.".into(),
        other => format!("Something went wrong: {other}"),
    }
}
