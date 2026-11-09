//! Short task titles from OpenRouter, when the user stored a key in Settings > General.
//! Blocking: run `generate` on a background thread.
use serde_json::{Value, json};
use std::process::Command;

pub const KEYCHAIN: &str = "https://openrouter.ai";
pub const KEYCHAIN_USER: &str = "openrouter";
const MODEL: &str = "deepseek/deepseek-v4.1-flash";
const URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const SYSTEM: &str = "Write a 3-4 word title for this coding task so the user can tell it apart \
    from their other tasks. Reply with only the title: no quotes, no trailing punctuation.";
/// A pasted log only adds cost: the gist is near the top.
const MAX_PROMPT: usize = 2000;

/// The key from a Keychain read; an error (a declined prompt) counts as none.
pub fn key_from(read: anyhow::Result<Option<(String, Vec<u8>)>>) -> Option<String> {
    (read.ok().flatten()).and_then(|(_, bytes)| String::from_utf8(bytes).ok())
}

/// A prompt of four words or fewer already is its title.
pub fn wanted(prompt: &str) -> bool {
    prompt.split_whitespace().nth(4).is_some()
}

fn body(prompt: &str) -> String {
    let prompt: String = prompt.trim().chars().take(MAX_PROMPT).collect();
    json!({
        "model": MODEL,
        "max_tokens": 20,
        "temperature": 0.2,
        "reasoning": { "enabled": false },
        "messages": [
            { "role": "system", "content": SYSTEM },
            { "role": "user", "content": prompt },
        ],
    })
    .to_string()
}

/// The model's title, tidied; `None` for anything that isn't one.
fn parse(response: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(response).ok()?;
    let text = v["choices"][0]["message"]["content"].as_str()?;
    let line = text.trim().lines().next()?;
    let line = line.trim_matches(|c: char| "\"'`*#.:".contains(c) || c.is_whitespace());
    let words: Vec<&str> = line.split_whitespace().take(6).collect();
    let title = words.join(" ");
    (!title.is_empty() && title.chars().count() <= 60).then_some(title)
}

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn generate(key: &str, prompt: &str) -> Result<String, String> {
    use std::os::unix::fs::DirBuilderExt;
    // Anything else could smuggle a second header line.
    if key.is_empty() || !key.bytes().all(|b| (0x21..0x7f).contains(&b)) {
        return Err("the OpenRouter key contains invalid characters".into());
    }
    let mut rand = [0u8; 16];
    getrandom::fill(&mut rand).map_err(|e| e.to_string())?;
    let hex: String = rand.iter().map(|b| format!("{b:02x}")).collect();
    let dir = Scratch(std::env::temp_dir().join(format!("shark-title-{hex}")));
    (std::fs::DirBuilder::new().mode(0o700))
        .create(&dir.0)
        .map_err(|e| e.to_string())?;
    // The key sits in an owner-only file, not in argv where `ps` shows it.
    let headers = format!("Content-Type: application/json\nAuthorization: Bearer {key}\n");
    crate::state::write(&dir.0.join("headers"), headers.as_bytes()).map_err(|e| e.to_string())?;
    crate::state::write(&dir.0.join("body"), body(prompt).as_bytes()).map_err(|e| e.to_string())?;
    let args = [
        // Must come first: ignores ~/.curlrc, which could add credentials or headers.
        "-q",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--connect-timeout",
        "10",
        "--max-time",
        "20",
        "--max-filesize",
        "1000000",
        "--user-agent",
        "Shark",
        "--header",
        "@headers",
        "--data-binary",
        "@body",
        "--output",
        "response",
        "--write-out",
        "%{http_code}",
        "--",
        URL,
    ];
    let mut c = Command::new("curl");
    c.env_remove("NETRC").env_remove("CURL_HOME");
    let status = crate::proc::wait_bounded(c, &dir.0, "curl", &args, 25, false)?;
    let response = std::fs::read(dir.0.join("response")).unwrap_or_default();
    if status != "200" {
        return Err(format!("OpenRouter answered HTTP {status}"));
    }
    parse(&response).ok_or_else(|| "OpenRouter sent no title".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(content: &str) -> Vec<u8> {
        json!({ "choices": [{ "message": { "content": content } }] })
            .to_string()
            .into_bytes()
    }

    #[test]
    fn tidies_the_reply() {
        assert_eq!(
            parse(&reply("Auto title new tasks")).as_deref(),
            Some("Auto title new tasks")
        );
        assert_eq!(
            parse(&reply("\"Fix Login Bug.\"\n\nextra")).as_deref(),
            Some("Fix Login Bug")
        );
        assert_eq!(
            parse(&reply("**Add OpenRouter titles**")).as_deref(),
            Some("Add OpenRouter titles")
        );
        assert_eq!(parse(&reply("   ")), None);
        assert_eq!(parse(b"{\"error\":{}}"), None);
        assert_eq!(parse(b"not json"), None);
    }

    #[test]
    fn short_prompts_keep_their_text() {
        assert!(!wanted("fix login bug"));
        assert!(!wanted("  one two three four "));
        assert!(wanted("when launching a new task add a title"));
    }

    #[test]
    fn body_carries_model_and_capped_prompt() {
        let v: Value = serde_json::from_str(&body(&"x".repeat(5000))).unwrap();
        assert_eq!(v["model"], MODEL);
        assert_eq!(
            v["messages"][1]["content"].as_str().unwrap().len(),
            MAX_PROMPT
        );
    }

    #[test]
    fn rejects_header_smuggling() {
        assert!(generate("sk\nX-Evil: 1", "a b c d e").is_err());
        assert!(generate("", "a b c d e").is_err());
    }

    /// `OPENROUTER_KEY=sk-or-… cargo test title::tests::live -- --ignored`
    #[test]
    #[ignore]
    fn live() {
        let key = std::env::var("OPENROUTER_KEY").unwrap();
        let title = generate(
            &key,
            "When launching a new task, name it with a short LLM title",
        );
        let title = title.unwrap();
        assert!(
            (1..=6).contains(&title.split_whitespace().count()),
            "{title}"
        );
    }
}
