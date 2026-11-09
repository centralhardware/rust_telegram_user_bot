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
//! The model runs behind a small HTTP service (`PROOFREAD_URL`, in dotclaude:
//! `scripts/system/proofread-server.py`) that drives headless Claude Code on the
//! Pro subscription, so there is no API key here. Off unless `PROOFREAD_URL`
//! is set. It runs in the background, so a message is saved without waiting.

use std::sync::OnceLock;
use std::time::Duration;

use grammers_client::update::Message;
use log::{info, warn};
use serde::Deserialize;

const TIMEOUT: Duration = Duration::from_secs(60);
/// Too short to be worth a call.
const MIN_CHARS: usize = 3;
/// Long texts are rarely typed by hand, and the call costs more.
const MAX_CHARS: usize = 4000;

struct Config {
    url: String,
    skip_chats: Vec<i64>,
    http: reqwest::Client,
}

fn config() -> Option<&'static Config> {
    static CONFIG: OnceLock<Option<Config>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let url = std::env::var("PROOFREAD_URL")
                .ok()
                .filter(|u| !u.is_empty())?;
            let skip_chats = std::env::var("PROOFREAD_SKIP_CHATS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            let http = reqwest::Client::builder().timeout(TIMEOUT).build().ok()?;
            info!("Proofreading outgoing messages via {url}");
            Some(Config {
                url,
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
            info!("Proofreading: rejected {}", word_diff(&text, &fixed));
            return;
        }
        match message.edit(fixed.as_str()).await {
            Ok(()) => info!("Proofreading: {}", word_diff(&text, &fixed)),
            Err(e) => warn!("Proofreading: failed to edit the message: {e}"),
        }
    });
}

#[derive(Deserialize)]
struct Response {
    text: String,
}

async fn ask(cfg: &Config, text: &str) -> anyhow::Result<String> {
    let response: Response = cfg
        .http
        .post(&cfg.url)
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let fixed = response.text.trim();
    Ok(if fixed.is_empty() { text } else { fixed }.to_string())
}

/// Unchanged words kept on each side of a change; longer unchanged runs fold into "…".
const DIFF_CONTEXT: usize = 3;

/// A one-line word diff for the log: `разница [-относиться-]{+относится,+} кроме`.
/// Punctuation sticks to its word, so an added comma shows as that word replaced.
pub fn word_diff(old: &str, new: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_words(old, new);
    // Group consecutive changes so a replaced word reads [-old-]{+new+}.
    let mut out: Vec<String> = Vec::new();
    let mut same: Vec<&str> = Vec::new();
    let (mut del, mut ins) = (String::new(), String::new());
    let flush_change = |out: &mut Vec<String>, del: &mut String, ins: &mut String| {
        if !del.trim().is_empty() {
            out.push(format!("[-{}-]", del.trim()));
        }
        if !ins.trim().is_empty() {
            out.push(format!("{{+{}+}}", ins.trim()));
        }
        del.clear();
        ins.clear();
    };
    let flush_same = |out: &mut Vec<String>, same: &mut Vec<&str>, first: bool, last: bool| {
        let words: Vec<&str> = same
            .iter()
            .copied()
            .filter(|w| !w.trim().is_empty())
            .collect();
        let keep_head = if first { 0 } else { DIFF_CONTEXT };
        let keep_tail = if last { 0 } else { DIFF_CONTEXT };
        if words.len() > keep_head + keep_tail {
            out.extend(words[..keep_head].iter().map(|w| w.to_string()));
            out.push("…".into());
            out.extend(
                words[words.len() - keep_tail..]
                    .iter()
                    .map(|w| w.to_string()),
            );
        } else {
            out.extend(words.iter().map(|w| w.to_string()));
        }
        same.clear();
    };
    let mut first = true;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if !del.is_empty() || !ins.is_empty() {
                    flush_change(&mut out, &mut del, &mut ins);
                }
                same.push(change.value());
            }
            tag => {
                if !same.is_empty() {
                    flush_same(&mut out, &mut same, first, false);
                }
                first = false;
                if tag == ChangeTag::Delete {
                    del.push_str(change.value())
                } else {
                    ins.push_str(change.value())
                }
            }
        }
    }
    flush_change(&mut out, &mut del, &mut ins);
    if !same.is_empty() {
        flush_same(&mut out, &mut same, first, true);
    }
    out.join(" ")
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
    // Hyphens split words too, so "кто то" → "кто-то" keeps both halves.
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| c.is_whitespace() || matches!(c, '-' | '‐' | '‑'))
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
    use super::{is_minor_fix, word_diff};

    #[test]
    fn log_diff() {
        assert_eq!(
            word_diff(
                "Какая вам лично разница кто куда формально относиться кроме еще одного повода чувствовать гордость",
                "Какая вам лично разница, кто куда формально относится, кроме еще одного повода чувствовать гордость"
            ),
            "Какая вам лично [-разница-] {+разница,+} кто куда формально [-относиться-] {+относится,+} кроме еще одного …"
        );
        assert_eq!(
            word_diff("првиет мир", "привет мир"),
            "[-првиет-] {+привет+} мир"
        );
    }

    #[test]
    fn accepts_typos_and_punctuation() {
        assert!(is_minor_fix("привет как дела", "привет, как дела?"));
        assert!(is_minor_fix(
            "купил айфон в apple stroe",
            "купил айфон в Apple Store"
        ));
        assert!(is_minor_fix("првиет мир", "привет мир"));
        assert!(is_minor_fix("ну да а что", "ну да — а что?"));
        assert!(is_minor_fix(
            "кто то пользуется study russian bot ?",
            "кто-то пользуется Study Russian Bot?"
        ));
    }

    #[test]
    fn rejects_rewrites() {
        assert!(!is_minor_fix("я пошел домой", "я отправился домой"));
        assert!(!is_minor_fix("пошел домой", "я пошел домой"));
        assert!(!is_minor_fix("я пошел домой", "домой я пошел"));
        assert!(!is_minor_fix("ну и ладно", "ну ладно"));
    }
}
