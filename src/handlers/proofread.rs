//! Proofreads the account's own outgoing text messages with Claude and edits
//! a message in place when it has typos, missing punctuation or misspelled
//! words and names.
//!
//! The model is told to fix only those, but its answer is not trusted:
//! [`is_minor_fix`] checks it, and anything more than small edits inside single
//! words, plus punctuation, is thrown away. The text must keep its words in the
//! same order. No word may be added, dropped, merged, split or swapped for
//! another, and no sentence rephrased.
//!
//! Off unless `ANTHROPIC_API_KEY` is set. It runs in the background, so a
//! message is saved without waiting on the model.

use std::sync::OnceLock;
use std::time::Duration;

use grammers_client::update::Message;
use log::{info, warn};
use serde::Deserialize;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const DEFAULT_MODEL: &str = "claude-haiku-5-5";
const TIMEOUT: Duration = Duration::from_secs(30);
/// Too short to be worth a call.
const MIN_CHARS: usize = 3;
/// Long texts are rarely typed by hand, and the call costs more.
const MAX_CHARS: usize = 4000;

const SYSTEM: &str = "You are a proofreader. The user message is a chat message someone just sent. \
Fix ONLY: typos, misspelled words, misspelled names/titles/brands, wrong letter case of proper names, \
and missing or wrong punctuation. Do NOT rephrase, do NOT change word order, do NOT add, remove, \
merge, split or replace words, do NOT fix grammar beyond a single word's spelling or ending, \
do NOT change style, slang, swearing, emoji, links, code or formatting, do NOT translate. \
Casual chat style is fine: do not add a final period, and do not capitalize the first letter \
if the author did not. If nothing needs fixing, return the text unchanged. \
Reply with the corrected text only, no quotes, no comments.";

struct Config {
    key: String,
    model: String,
    skip_chats: Vec<i64>,
    http: reqwest::Client,
}

fn config() -> Option<&'static Config> {
    static CONFIG: OnceLock<Option<Config>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let key = std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())?;
            let model =
                std::env::var("PROOFREAD_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
            let skip_chats = std::env::var("PROOFREAD_SKIP_CHATS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            let http = reqwest::Client::builder().timeout(TIMEOUT).build().ok()?;
            info!("Proofreading outgoing messages with {model}");
            Some(Config {
                key,
                model,
                skip_chats,
                http,
            })
        })
        .as_ref()
}

/// Starts proofreading an outgoing message, if it is one worth checking.
pub fn proofread_outgoing(message: &Message) {
    let Some(cfg) = config() else { return };
    let text = message.text();
    let chars = text.chars().count();
    if !(MIN_CHARS..=MAX_CHARS).contains(&chars)
        || text.starts_with('/')
        || message.forward_header().is_some()
        || message.via_bot_id().is_some()
        // Editing with plain text would drop bold, links, mentions and the like.
        || message.fmt_entities().is_some_and(|e| !e.is_empty())
        || cfg.skip_chats.contains(&message.peer_id().bare_id_unchecked())
    {
        return;
    }
    let message = message.clone();
    let text = text.to_string();
    tokio::spawn(async move {
        let fixed = match ask(cfg, &text).await {
            Ok(fixed) => fixed,
            Err(e) => {
                warn!("Proofreading failed: {e:#}");
                return;
            }
        };
        if fixed == text {
            return;
        }
        if !is_minor_fix(&text, &fixed) {
            info!("Proofreading: rejected a rewrite of {text:?} into {fixed:?}");
            return;
        }
        match message.edit(fixed.as_str()).await {
            Ok(()) => info!("Proofreading: {text:?} -> {fixed:?}"),
            Err(e) => warn!("Proofreading: failed to edit the message: {e}"),
        }
    });
}

#[derive(Deserialize)]
struct Response {
    content: Vec<Block>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(default)]
    text: String,
}

async fn ask(cfg: &Config, text: &str) -> anyhow::Result<String> {
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 4096,
        "system": SYSTEM,
        "messages": [{ "role": "user", "content": text }],
    });
    let response: Response = cfg
        .http
        .post(API_URL)
        .header("x-api-key", &cfg.key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let fixed: String = response.content.into_iter().map(|b| b.text).collect();
    let fixed = fixed.trim();
    Ok(if fixed.is_empty() { text } else { fixed }.to_string())
}

/// The word in a token, without the punctuation around or inside it.
fn core(token: &str) -> String {
    token.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Whether `fixed` differs from `original` only by punctuation and by small
/// edits inside single words: the same words in the same order, each spelled
/// at most a couple of letters differently.
pub fn is_minor_fix(original: &str, fixed: &str) -> bool {
    // Tokens that are only punctuation (a dash, an ellipsis) may come and go.
    let words = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(core)
            .filter(|w| !w.is_empty())
            .collect()
    };
    let (a, b) = (words(original), words(fixed));
    a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| is_word_fix(x, y))
}

fn is_word_fix(a: &str, b: &str) -> bool {
    if a == b || a.to_lowercase() == b.to_lowercase() {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (
        a.to_lowercase().chars().collect(),
        b.to_lowercase().chars().collect(),
    );
    let longest = a.len().max(b.len());
    // One letter in short words, up to a third of the letters in long ones.
    let allowed = (longest / 3).clamp(1, 3);
    distance(&a, &b) <= allowed
}

/// Damerau-Levenshtein (optimal string alignment): a swap of two adjacent
/// letters, the commonest typo, counts as one edit.
fn distance(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

#[cfg(test)]
mod tests {
    use super::is_minor_fix;

    #[test]
    fn accepts_typos_and_punctuation() {
        assert!(is_minor_fix("привет как дела", "привет, как дела?"));
        assert!(is_minor_fix(
            "купил айфон в apple stroe",
            "купил айфон в Apple Store"
        ));
        assert!(is_minor_fix("првиет мир", "привет мир"));
        assert!(is_minor_fix("ну да а что", "ну да — а что?"));
    }

    #[test]
    fn rejects_rewrites() {
        assert!(!is_minor_fix("я пошел домой", "я отправился домой"));
        assert!(!is_minor_fix("пошел домой", "я пошел домой"));
        assert!(!is_minor_fix("я пошел домой", "домой я пошел"));
        assert!(!is_minor_fix("ну и ладно", "ну ладно"));
    }
}
