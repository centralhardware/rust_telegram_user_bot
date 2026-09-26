//! The dialog list, read page by page through the raw `messages.getDialogs`.
//!
//! `Client::iter_dialogs` is not used: it panics -- "dialogs use an unknown
//! peer" -- on a dialog whose peer the same response did not name, and
//! `dialogCommunity` names none at all, so the panic takes the whole process
//! down as soon as the account belongs to a community. Nothing here needs a
//! grammers `Dialog` anyway: the ids, access hashes and titles a caller wants
//! are on the `chats` and `users` of each page.

use std::time::Duration;

use grammers_client::Client;
use grammers_session::types::PeerRef;
use grammers_tl_types as tl;
use log::warn;

/// The main dialog list, and the archive beside it. A request names one of
/// them: asked for neither, Telegram answers with the main list and the
/// archive is simply missing.
pub const MAIN_FOLDER: i32 = 0;
pub const ARCHIVE_FOLDER: i32 = 1;

/// How many dialogs a page asks for. Telegram's own limit for `getDialogs`.
const PAGE: i32 = 100;
/// The pause between pages, so a long list is not read fast enough to be
/// rate-limited.
const PAGE_GAP: Duration = Duration::from_millis(500);

/// One page of a folder: its dialogs and the chats and users they name.
pub struct Page {
    pub dialogs: Vec<tl::enums::Dialog>,
    pub chats: Vec<tl::enums::Chat>,
    pub users: Vec<tl::enums::User>,
}

/// A folder's dialogs, one page at a time. A chat can come on more than one
/// page -- the pinned ones come with the first page and again in place on a
/// later one -- so a caller keeps its own set of the ones it has seen.
pub struct Pages<'a> {
    client: &'a Client,
    request: tl::functions::messages::GetDialogs,
    started: bool,
    done: bool,
}

impl<'a> Pages<'a> {
    pub fn new(client: &'a Client, folder_id: i32) -> Self {
        Pages {
            client,
            request: tl::functions::messages::GetDialogs {
                exclude_pinned: false,
                folder_id: Some(folder_id),
                offset_date: 0,
                offset_id: 0,
                offset_peer: tl::enums::InputPeer::Empty,
                limit: PAGE,
                hash: 0,
            },
            started: false,
            done: false,
        }
    }

    /// The next page, or `None` once the folder is read to the end.
    pub async fn next(&mut self) -> Result<Option<Page>, grammers_client::InvocationError> {
        if self.done {
            return Ok(None);
        }
        if self.started {
            tokio::time::sleep(PAGE_GAP).await;
        }
        self.started = true;

        use tl::enums::messages::Dialogs;
        let (dialogs, messages, chats, users, last_page) =
            match self.client.invoke(&self.request).await? {
                Dialogs::Dialogs(d) => (d.dialogs, d.messages, d.chats, d.users, true),
                Dialogs::Slice(d) => {
                    let last = d.dialogs.len() < self.request.limit as usize;
                    (d.dialogs, d.messages, d.chats, d.users, last)
                }
                // Only returned for a non-zero `hash`, which this never sends.
                Dialogs::NotModified(_) => {
                    self.done = true;
                    return Ok(None);
                }
            };

        self.done = last_page || !self.advance(&dialogs, &messages, &chats, &users);
        Ok(Some(Page { dialogs, chats, users }))
    }

    /// Move the request on to the page after this one. False when there is
    /// nowhere to move it to.
    ///
    /// Where the next page starts: the last dialog of this one that can be
    /// paged from at all. A community names no peer and holds no message, and
    /// a peer the response did not describe cannot be addressed -- and
    /// `InputPeerEmpty` would page from the top again rather than skip ahead.
    ///
    /// Which dialog is a fit offset has nothing to do with which one a caller
    /// wants: a chat this account was thrown out of is skipped as a chat and
    /// is still perfectly good as a place in the list.
    ///
    /// The date has to be the dialog's own. Telegram pages this list by date
    /// above all, and a dialog whose top message the response did not carry --
    /// deleted since, so the response holds a `messageEmpty` for it -- has none
    /// to offer. Carrying the last page's date over would ask for the dialogs
    /// below a point already passed, and everything between the two is never
    /// asked for at all. So a dialog that cannot say when it last spoke is not
    /// the offset either, and the scan steps back to one that can -- at worst
    /// re-reading a dialog it has already seen.
    fn advance(
        &mut self,
        dialogs: &[tl::enums::Dialog],
        messages: &[tl::enums::Message],
        chats: &[tl::enums::Chat],
        users: &[tl::enums::User],
    ) -> bool {
        let Some((offset_id, offset_date, offset_peer)) =
            dialogs.iter().rev().find_map(|dialog| {
                let (peer, top_message) = dialog_offset(dialog)?;
                let peer = address(&peer, chats, users)?;
                let date = messages
                    .iter()
                    .find(|m| m.id() == top_message)
                    .and_then(message_date)?;
                Some((top_message, date, peer))
            })
        else {
            return false;
        };
        // An offset that did not move would ask for the same page forever.
        if self.request.offset_id == offset_id && self.request.offset_date == offset_date {
            warn!("dialog paging stopped moving at message {offset_id}");
            return false;
        }
        self.request.offset_id = offset_id;
        self.request.offset_date = offset_date;
        self.request.offset_peer = offset_peer;
        // The pinned dialogs came with the first page.
        self.request.exclude_pinned = true;
        true
    }
}

/// The peer and top message a dialog can be paged from, for the kinds that have
/// one. `dialogCommunity` has neither.
pub fn dialog_offset(dialog: &tl::enums::Dialog) -> Option<(tl::enums::Peer, i32)> {
    match dialog {
        tl::enums::Dialog::Dialog(d) => Some((d.peer.clone(), d.top_message)),
        tl::enums::Dialog::Folder(d) => Some((d.peer.clone(), d.top_message)),
        tl::enums::Dialog::Community(_) => None,
    }
}

/// The `InputPeer` for a dialog's peer, out of the chats and users of the same
/// response -- whatever kind of chat it is. `None` only for a peer the response
/// did not describe.
pub fn address(
    peer: &tl::enums::Peer,
    chats: &[tl::enums::Chat],
    users: &[tl::enums::User],
) -> Option<tl::enums::InputPeer> {
    match peer {
        tl::enums::Peer::User(p) => users
            .iter()
            .find(|u| u.id() == p.user_id)
            .map(|u| PeerRef::from(u).into()),
        tl::enums::Peer::Chat(p) => chats
            .iter()
            .find(|c| c.id() == p.chat_id)
            .map(|c| PeerRef::from(c).into()),
        tl::enums::Peer::Channel(p) => chats
            .iter()
            .find(|c| c.id() == p.channel_id)
            .map(|c| PeerRef::from(c).into()),
    }
}

/// When a message was sent, for the kinds that were sent at a time at all.
fn message_date(message: &tl::enums::Message) -> Option<i32> {
    match message {
        tl::enums::Message::Message(m) => Some(m.date),
        tl::enums::Message::Service(m) => Some(m.date),
        tl::enums::Message::Empty(_) => None,
    }
}
