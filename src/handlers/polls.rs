//! How a poll stands after a vote.
//!
//! Telegram reports a poll the way it reports reactions: not "someone voted for
//! the second option" but "the counts are now this". So a row is that snapshot,
//! under `event = 'poll'` — the per-option voter counts and the total.
//!
//! The update names the message only sometimes; when it does not, `poll_id` is
//! the only link back, which is why the send row stores it too. An option is
//! keyed by the opaque bytes Telegram identifies it by, rendered as text when
//! they are text and as hex when they are not — the option's wording is on the
//! send row, in `poll_options`, in the order the poll was created with.

use grammers_client::session::types::PeerId;
use grammers_tl_types as tl;
use log::info;

use crate::db::{EVENTS_BUF, Event};
use crate::utils::log_ignore::is_log_ignored;
use crate::utils::peer_names::title_of;

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
    let chat_id = peer
        .as_ref()
        .map(|p| p.bare_id_unchecked())
        .unwrap_or_default();

    let (question, options) = match &update.poll {
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

    if !is_log_ignored(chat_id) {
        let chat_short: String = match &peer {
            Some(p) => title_of(p.bot_api_dialog_id_unchecked()).await,
            None => update.poll_id.to_string(),
        }
        .chars()
        .take(25)
        .collect();
        let rendered = counts
            .iter()
            .map(|(option, voters)| format!("{option}×{voters}"))
            .collect::<Vec<_>>()
            .join(" ");
        info!(
            "\x1b[96m{:<8} {:>8} {:<25} \x1b[90m│\x1b[96m {}\x1b[0m",
            "poll",
            update.msg_id.unwrap_or(0),
            chat_short,
            rendered,
        );
    }

    EVENTS_BUF
        .push(Event {
            date_time: chrono::Utc::now().timestamp() as u32,
            chat_id,
            message_id: update.msg_id.unwrap_or(0) as i64,
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
    use super::option_key;

    #[test]
    fn an_option_is_keyed_by_its_bytes_as_text_when_they_are_text() {
        assert_eq!(option_key(b"0"), "0");
        assert_eq!(option_key(b"yes"), "yes");
    }

    #[test]
    fn and_by_their_hex_when_they_are_not() {
        assert_eq!(option_key(&[0x00, 0xff]), "00ff");
    }
}
