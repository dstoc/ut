use crate::config::Config;
use crate::context::AppContext;

const DEFAULT_PROMPT_NAME: &str = "default";
const BUILTIN_PROMPT_TEXT: &str = "You are a dictation engine.\n\
Return only the final insertable text.\n\
Remove filler words, repeated fragments, and obvious false starts.\n\
Choose the best format for the content:\n\
- Shell command: Format for execution as a shell command, consider whitespace, convert slash => /, pipe => |, tilde => ~, etc. No markdown, code fences or commentary.\n\
- Code: Correct indentation. No markdown, code fences, or commentary.\n\
- Chat message: Informal text, light on formatting, detect/convert emoji.\n\
- Prose: Add quotes, markdown, bullet points/lists, code fences, and other formatting as necessary.\n\
Follow and discard any formatting instructions in the content first.";

pub fn build_prompt(context: &AppContext, config: &Config) -> String {
    let prompt_name = config
        .prompt_name_for_context(context)
        .unwrap_or(DEFAULT_PROMPT_NAME);
    config
        .prompts
        .get(prompt_name)
        .cloned()
        .unwrap_or_else(|| BUILTIN_PROMPT_TEXT.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppRule, Config};

    #[test]
    fn prompt_defaults_to_builtin_text() {
        let prompt = build_prompt(&AppContext::default(), &Config::default());
        assert!(prompt.contains("You are a dictation engine."));
        assert!(prompt.contains("insertable text"));
    }

    #[test]
    fn prompt_can_be_overridden_by_name() {
        let mut config = Config::default();
        config
            .prompts
            .insert("terminal".to_string(), "terminal prompt".to_string());
        config.app_rules = vec![AppRule {
            app_id: Some("kitty".to_string()),
            class: None,
            title_contains: None,
            prompt: Some("terminal".to_string()),
            paste_keys: None,
        }];

        let context = AppContext {
            app_id: Some("kitty".to_string()),
            ..Default::default()
        };

        assert_eq!(build_prompt(&context, &config), "terminal prompt");
    }

    #[test]
    fn missing_named_prompt_falls_back_to_builtin_text() {
        let config = Config {
            app_rules: vec![AppRule {
                app_id: Some("kitty".to_string()),
                class: None,
                title_contains: None,
                prompt: Some("terminal".to_string()),
                paste_keys: None,
            }],
            ..Default::default()
        };

        let context = AppContext {
            app_id: Some("kitty".to_string()),
            ..Default::default()
        };

        let prompt = build_prompt(&context, &config);
        assert!(prompt.contains("You are a dictation engine."));
    }

    #[test]
    fn config_default_prompt_overrides_builtin_default() {
        let mut config = Config::default();
        config
            .prompts
            .insert("default".to_string(), "custom default prompt".to_string());

        assert_eq!(
            build_prompt(&AppContext::default(), &config),
            "custom default prompt"
        );
    }
}
