//! How a poll stands after a vote.
//!
//! Telegram reports a poll the way it reports reactions: not "someone voted for
//! the second option" but "the counts are now this". So a row is that snapshot,
//! under `event = 'poll'` — the per-option voter counts and the total.
//!
//! The update names the message, its chat and the poll itself only sometimes;
//! when it does not, `poll_id` is the only link back, which is why the send row
//! stores it too. What the update leaves out is read back from that row through
//! `poll_info`, so the row written here — and the console line — name the chat,
//! the question and the options rather than an id and a row of indexes. An
//! option is keyed by the opaque bytes Telegram identifies it by, rendered as
//! text when they are text and as hex when they are not; the wording lines up
//! with the counts by position, in the order the poll was created with.

use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;
use log::info;

use crate::db::{EVENTS_BUF, Event};
use crate::utils::log_ignore::is_log_ignored;
use crate::utils::peer_names::title_of;
use crate::utils::poll_info;

pub async fn save_poll(update: &tl::types::UpdateMessagePoll) {
    let tl::enums::PollResults::Results(results) = &update.results;

    // A "min" result set is the poll as seen without the account's own vote
    // applied; the counts in it are still the real ones, so it is logged like
    // any other.
    let Some(answers) = results.results.as_ref() else {
        return;
    };

    let counts: Vec<(String, u32)> = answers
        .iter()
        .map(|answer| {
            let tl::enums::PollAnswerVoters::Voters(voters) = answer;
            (
                option_key(&voters.option),
                voters.voters.unwrap_or(0).max(0) as u32,
            )
        })
        .collect();

    let peer = update.peer.as_ref().map(PeerId::from);
    let mut chat_id = peer
        .as_ref()
        .map(|p| p.bare_id_unchecked())
        .unwrap_or_default();

    let (mut question, mut options) = match &update.poll {
        Some(tl::enums::Poll::Poll(poll)) => {
            let tl::enums::TextWithEntities::Entities(q) = &poll.question;
            let options = poll
                .answers
                .iter()
                .filter_map(|a| match a {
                    tl::enums::PollAnswer::Answer(a) => {
                        let tl::enums::TextWithEntities::Entities(t) = &a.text;
                        Some(t.text.clone())
                    }
                    _ => None,
                })
                .collect();
            (q.text.clone(), options)
        }
        None => (String::new(), Vec::new()),
    };

    // A results update usually carries neither the poll nor its peer — only the
    // poll id. The send row has the rest, so the row this one writes says what
    // was voted on rather than leaving the reader an id and a row of indexes.
    let mut chat_title = String::new();
    let mut message_id = update.msg_id.unwrap_or(0) as i64;
    if question.is_empty() || peer.is_none() || message_id == 0 {
        if let Some(info) = poll_info::load(update.poll_id).await {
            if question.is_empty() {
                question = info.question;
                options = info.options;
            }
            if chat_id == 0 {
                chat_id = info.chat_id;
            }
            if message_id == 0 {
                message_id = info.message_id;
            }
            chat_title = info.chat_title;
        }
    }

    if !is_log_ignored(chat_id) {
        let title = match &peer {
            Some(p) => title_of(p.bot_api_dialog_id_unchecked()).await,
            None => String::new(),
        };
        let title = match title {
            t if !t.is_empty() => t,
            _ if !chat_title.is_empty() => chat_title.clone(),
            _ => update.poll_id.to_string(),
        };
        let chat_short: String = title.chars().take(25).collect();
        let rendered = render_counts(&counts, &options);
        let rendered = if question.is_empty() {
            rendered
        } else {
            let q: String = question.replace('\n', " ").chars().take(40).collect();
            format!("{q} \x1b[90m—\x1b[96m {rendered}")
        };
        info!(
            "\x1b[96m{:<8} {:>8} {:<25} \x1b[90m│\x1b[96m {}\x1b[0m",
            "poll",
            message_id,
            chat_short,
            rendered,
        );
    }

    EVENTS_BUF
        .push(Event {
            date_time: chrono::Utc::now().timestamp() as u32,
            chat_id,
            message_id,
            topic_id: update.top_msg_id.unwrap_or(0),
            poll_id: update.poll_id,
            poll_question: question,
            poll_options: options,
            poll_results: counts,
            poll_total_voters: results.total_voters.unwrap_or(0).max(0) as u32,
            ..Event::poll()
        })
        .await;
}

/// The counts as "wording×voters". The results come in the poll's own answer
/// order, so the wording lines up by position; an option whose wording is not
/// known — a poll whose message was never seen — keeps its key.
fn render_counts(counts: &[(String, u32)], options: &[String]) -> String {
    counts
        .iter()
        .enumerate()
        .map(|(i, (option, voters))| {
            let label = options.get(i).filter(|o| !o.is_empty()).unwrap_or(option);
            format!("{label}×{voters}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The option's own bytes when they are readable text — which is what a client
/// sending a poll normally uses — and their hex otherwise.
fn option_key(option: &[u8]) -> String {
    match std::str::from_utf8(option) {
        Ok(text) if text.chars().all(|c| !c.is_control()) => text.to_string(),
        _ => option.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{option_key, render_counts};

    #[test]
    fn an_option_is_keyed_by_its_bytes_as_text_when_they_are_text() {
        assert_eq!(option_key(b"0"), "0");
        assert_eq!(option_key(b"yes"), "yes");
    }

    #[test]
    fn and_by_their_hex_when_they_are_not() {
        assert_eq!(option_key(&[0x00, 0xff]), "00ff");
    }

    fn counts() -> Vec<(String, u32)> {
        vec![("0".into(), 8901), ("1".into(), 9144)]
    }

    #[test]
    fn a_count_is_shown_under_the_option_in_the_same_position() {
        let options = vec!["Yes".to_string(), "No".to_string()];
        assert_eq!(render_counts(&counts(), &options), "Yes×8901 No×9144");
    }

    #[test]
    fn and_under_its_key_when_the_wording_is_not_known() {
        assert_eq!(render_counts(&counts(), &[]), "0×8901 1×9144");
        let partial = vec![String::new(), "No".to_string()];
        assert_eq!(render_counts(&counts(), &partial), "0×8901 No×9144");
    }
}
