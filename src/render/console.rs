//! The console line every event prints: one layout, one set of colours, one
//! truncation rule. Handlers fill a [`LogLine`]; this module owns how it looks.
//!
//! ```text
//! [12:00:00] incoming  3605193 Pro Hi-Tech Chat          │ Rose       │ Hello
//!            └ kind ─┘ └─ id ─┘ └─ chat, 25 ─────────────┘  └ sender ┘  └ body
//! ```
//!
//! A line without a chat puts the body straight after the id, and a line
//! without a sender leaves that column out. A body of several lines continues
//! under its own first line, past the logger's timestamp, so nothing else can
//! land between them: the whole thing is one log record.

use std::fmt::Display;

/// What kind of event a line is about, which is what picks its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Incoming,
    Outgoing,
    Edited,
    Deleted,
    /// Reactions and service actions.
    Action,
    Ephemeral,
    /// Pins, polls, views, backfills: things that happened to a message
    /// rather than messages.
    Info,
    /// Context for another line — the message a reply answers.
    Muted,
}

impl Tone {
    fn code(self) -> u8 {
        match self {
            Tone::Incoming => 92,
            Tone::Outgoing | Tone::Action => 95,
            Tone::Edited => 93,
            Tone::Deleted => 91,
            Tone::Ephemeral => 94,
            Tone::Info => 96,
            Tone::Muted => GREY,
        }
    }
}

const GREY: u8 = 90;
const KIND_WIDTH: usize = 8;
const ID_WIDTH: usize = 8;
const CHAT_WIDTH: usize = 25;
const SENDER_WIDTH: usize = 10;
/// `[00:00:00] `, which the logger puts in front of every record.
const TIMESTAMP_WIDTH: usize = "[00:00:00] ".len();

pub struct LogLine<'a> {
    tone: Tone,
    kind: &'a str,
    id: String,
    chat: Option<&'a str>,
    sender: Option<&'a str>,
    body: &'a str,
}

impl<'a> LogLine<'a> {
    pub fn new(tone: Tone, kind: &'a str, id: impl Display) -> Self {
        LogLine {
            tone,
            kind,
            id: id.to_string(),
            chat: None,
            sender: None,
            body: "",
        }
    }

    /// The chat's title, cut to its column.
    pub fn chat(mut self, chat: &'a str) -> Self {
        self.chat = Some(chat);
        self
    }

    /// Who sent it, cut to its column.
    pub fn sender(mut self, sender: &'a str) -> Self {
        self.sender = Some(sender);
        self
    }

    /// What happened. May carry colours of its own, and may run to several
    /// lines.
    pub fn body(mut self, body: &'a str) -> Self {
        self.body = body;
        self
    }

    pub fn print(&self) {
        log::info!("{}", self.render());
    }

    pub fn render(&self) -> String {
        let c = self.tone.code();
        let colour = format!("\x1b[{c}m");
        let bar = format!("\x1b[{GREY}m│{colour}");
        let mut line = format!("{colour}{:<KIND_WIDTH$} {:>ID_WIDTH$}", self.kind, self.id);
        // How wide everything before the body is, for lining up its
        // continuation lines; the colour codes take no room.
        let mut width = KIND_WIDTH + 1 + ID_WIDTH;

        if let Some(chat) = self.chat {
            line.push_str(&format!(" {:<CHAT_WIDTH$}", clip(chat, CHAT_WIDTH)));
            width += 1 + CHAT_WIDTH;
            if let Some(sender) = self.sender {
                line.push_str(&format!(" {bar} {:<SENDER_WIDTH$}", clip(sender, SENDER_WIDTH)));
                width += 3 + SENDER_WIDTH;
            }
        }

        let mut lines = self.body.lines();
        if let Some(first) = lines.next() {
            if self.chat.is_some() {
                line.push_str(&format!(" {bar} {first}"));
            } else {
                line.push_str(&format!(" {first}"));
            }
        }
        for next in lines {
            line.push_str(&format!(
                "\n{:width$}{bar} {next}",
                "",
                width = TIMESTAMP_WIDTH + width + 1,
            ));
        }
        line.push_str("\x1b[0m");
        line
    }
}

/// A span picked out of a body — a quoted passage — in the accent colour,
/// then back to the line's own.
pub fn emphasize(text: &str, within: Tone) -> String {
    format!("\x1b[{}m{text}\x1b[{}m", Tone::Info.code(), within.code())
}

fn clip(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line as it would read with the colours taken out.
    fn plain(line: &LogLine) -> String {
        let rendered = line.render();
        let mut out = String::new();
        let mut chars = rendered.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(|&c| c == 'm');
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn every_column_is_its_own_width() {
        let line = LogLine::new(Tone::Incoming, "incoming", 42)
            .chat("chat")
            .sender("Rose")
            .body("hello");
        assert_eq!(
            plain(&line),
            format!("incoming       42 {:<25} │ {:<10} │ hello", "chat", "Rose")
        );
    }

    #[test]
    fn titles_and_names_are_cut_to_their_columns() {
        let line = LogLine::new(Tone::Deleted, "deleted", 1)
            .chat("a chat whose title goes on for a good while")
            .sender("Bartholomew")
            .body("x");
        let text = plain(&line);
        assert!(text.contains("a chat whose title goes o │ Bartholome │ x"), "{text}");
    }

    #[test]
    fn a_line_with_no_chat_puts_the_body_after_the_id() {
        let line = LogLine::new(Tone::Info, "backfill", "new").body("3 chats");
        assert_eq!(plain(&line), "backfill      new 3 chats");
    }

    #[test]
    fn nothing_trails_a_line_with_no_body() {
        let line = LogLine::new(Tone::Info, "pin", 7).chat("chat");
        assert_eq!(plain(&line), format!("pin             7 {:<25}", "chat"));
    }

    #[test]
    fn a_long_body_continues_under_its_first_line() {
        let line = LogLine::new(Tone::Info, "poll", 5).chat("chat").body("question\noption");
        let text = plain(&line);
        let [first, second] = text.lines().collect::<Vec<_>>().try_into().unwrap();
        // The continuation's bar sits under the first line's, once the
        // logger's timestamp is in front of the first.
        let bar = |s: &str| s.chars().collect::<Vec<_>>().iter().rposition(|&c| c == '│').unwrap();
        assert_eq!(bar(second), bar(first) + TIMESTAMP_WIDTH);
    }
}
