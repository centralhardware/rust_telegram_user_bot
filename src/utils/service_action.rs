use grammers_tl_types::enums::MessageAction;

pub fn format(action: &MessageAction) -> String {
    match action {
        MessageAction::Empty => "[service message]".into(),
        MessageAction::ChatCreate(a) => {
            let ids = format_ids(&a.users);
            format!("[chat created: \"{}\", members: {}]", a.title, ids)
        }
        MessageAction::ChatEditTitle(a) => format!("[title changed to \"{}\"]", a.title),
        MessageAction::ChatEditPhoto(_) => "[chat photo updated]".into(),
        MessageAction::ChatDeletePhoto => "[chat photo removed]".into(),
        MessageAction::ChatAddUser(a) => {
            format!("[users added: {}]", format_ids(&a.users))
        }
        MessageAction::ChatDeleteUser(a) => format!("[user removed: {}]", a.user_id),
        MessageAction::ChatJoinedByLink(a) => {
            format!("[joined via invite link from {}]", a.inviter_id)
        }
        MessageAction::ChatJoinedByRequest => "[joined by request]".into(),
        MessageAction::ChannelCreate(a) => format!("[channel created: \"{}\"]", a.title),
        MessageAction::ChatMigrateTo(a) => {
            format!("[migrated to supergroup {}]", a.channel_id)
        }
        MessageAction::ChannelMigrateFrom(a) => {
            format!("[supergroup created from chat \"{}\", chat {}]", a.title, a.chat_id)
        }
        MessageAction::PinMessage => "[message pinned]".into(),
        MessageAction::HistoryClear => "[history cleared]".into(),
        MessageAction::GameScore(a) => {
            format!("[game score: {} in game {}]", a.score, a.game_id)
        }
        MessageAction::PaymentSentMe(a) => {
            format!("[payment received: {} {}]", a.total_amount, a.currency)
        }
        MessageAction::PaymentSent(a) => {
            format!("[payment sent: {} {}]", a.total_amount, a.currency)
        }
        MessageAction::PhoneCall(a) => {
            let kind = if a.video { "video call" } else { "call" };
            match a.duration {
                Some(d) => format!("[{kind}, {d} sec]"),
                None => format!("[{kind}, no answer]"),
            }
        }
        MessageAction::ScreenshotTaken => "[screenshot taken]".into(),
        MessageAction::CustomAction(a) => format!("[{}]", a.message),
        MessageAction::BotAllowed(a) => match &a.domain {
            Some(d) => format!("[bot allowed, domain: {d}]"),
            None => "[bot allowed]".into(),
        },
        MessageAction::SecureValuesSentMe(_) => "[passport data received]".into(),
        MessageAction::SecureValuesSent(_) => "[passport data sent]".into(),
        MessageAction::ContactSignUp => "[joined Telegram]".into(),
        MessageAction::GeoProximityReached(a) => {
            format!("[proximity alert: {} m]", a.distance)
        }
        MessageAction::GroupCall(a) => match a.duration {
            Some(d) => format!("[group call, {d} sec]"),
            None => "[group call started]".into(),
        },
        MessageAction::InviteToGroupCall(a) => {
            format!("[invited to group call: {}]", format_ids(&a.users))
        }
        MessageAction::SetMessagesTtl(a) => {
            if a.period == 0 {
                "[auto-delete disabled]".into()
            } else {
                format!("[auto-delete: {} sec]", a.period)
            }
        }
        MessageAction::GroupCallScheduled(a) => {
            format!("[group call scheduled at {}]", a.schedule_date)
        }
        MessageAction::SetChatTheme(a) => format!("[chat theme changed: {:?}]", a.theme),
        MessageAction::WebViewDataSentMe(a) => {
            format!("[webview data received: {}]", a.text)
        }
        MessageAction::WebViewDataSent(a) => {
            format!("[webview data sent: {}]", a.text)
        }
        MessageAction::GiftPremium(a) => {
            format!("[gift Premium, {} days, {} {}]", a.days, a.amount, a.currency)
        }
        MessageAction::TopicCreate(a) => format!("[topic created: \"{}\"]", a.title),
        MessageAction::TopicEdit(a) => {
            let mut parts = Vec::new();
            if let Some(t) = &a.title {
                parts.push(format!("title: \"{t}\""));
            }
            if let Some(true) = a.closed {
                parts.push("closed".into());
            } else if let Some(false) = a.closed {
                parts.push("reopened".into());
            }
            if let Some(true) = a.hidden {
                parts.push("hidden".into());
            } else if let Some(false) = a.hidden {
                parts.push("unhidden".into());
            }
            if parts.is_empty() {
                "[topic edited]".into()
            } else {
                format!("[topic edited: {}]", parts.join(", "))
            }
        }
        MessageAction::SuggestProfilePhoto(_) => "[profile photo suggested]".into(),
        MessageAction::RequestedPeer(a) => {
            format!("[peer shared, button {}]", a.button_id)
        }
        MessageAction::SetChatWallPaper(a) => {
            let scope = if a.for_both { ", for both" } else { "" };
            format!("[wallpaper changed{scope}]")
        }
        MessageAction::GiftCode(a) => {
            format!("[gift code, {} days, slug: {}]", a.days, a.slug)
        }
        MessageAction::GiveawayLaunch(a) => match a.stars {
            Some(s) => format!("[giveaway launched, {s} stars]"),
            None => "[giveaway launched]".into(),
        },
        MessageAction::GiveawayResults(a) => {
            format!(
                "[giveaway results: {} winners, {} unclaimed]",
                a.winners_count, a.unclaimed_count
            )
        }
        MessageAction::BoostApply(a) => format!("[boost x{}]", a.boosts),
        MessageAction::RequestedPeerSentMe(a) => {
            format!("[peer shared to me, button {}]", a.button_id)
        }
        MessageAction::PaymentRefunded(a) => {
            format!("[payment refunded: {} {}]", a.total_amount, a.currency)
        }
        MessageAction::GiftStars(a) => {
            format!("[gift {} stars, {} {}]", a.stars, a.amount, a.currency)
        }
        MessageAction::PrizeStars(a) => {
            format!("[prize {} stars, giveaway msg {}]", a.stars, a.giveaway_msg_id)
        }
        MessageAction::StarGift(a) => {
            let mut s = "[star gift".to_string();
            if a.converted { s.push_str(", converted"); }
            if a.upgraded { s.push_str(", upgraded"); }
            if a.refunded { s.push_str(", refunded"); }
            s.push(']');
            s
        }
        MessageAction::StarGiftUnique(a) => {
            let mut s = "[unique star gift".to_string();
            if a.upgrade { s.push_str(", upgrade"); }
            if a.transferred { s.push_str(", transferred"); }
            if let Some(stars) = a.transfer_stars {
                s.push_str(&format!(", transfer cost: {stars}"));
            }
            s.push(']');
            s
        }
        MessageAction::PaidMessagesRefunded(a) => {
            format!("[paid messages refunded: {} msgs, {} stars]", a.count, a.stars)
        }
        MessageAction::PaidMessagesPrice(a) => {
            format!("[paid message price: {} stars]", a.stars)
        }
        MessageAction::ConferenceCall(a) => {
            let kind = if a.video { "video conference" } else { "conference call" };
            let status = if a.missed {
                ", missed"
            } else if a.active {
                ", active"
            } else {
                ""
            };
            match a.duration {
                Some(d) => format!("[{kind}{status}, {d} sec]"),
                None => format!("[{kind}{status}]"),
            }
        }
        MessageAction::TodoCompletions(a) => {
            format!(
                "[tasks: {} completed, {} uncompleted]",
                a.completed.len(),
                a.incompleted.len()
            )
        }
        MessageAction::TodoAppendTasks(a) => {
            format!("[{} tasks added]", a.list.len())
        }
        MessageAction::SuggestedPostApproval(a) => {
            if a.rejected {
                match &a.reject_comment {
                    Some(c) => format!("[suggested post rejected: {c}]"),
                    None => "[suggested post rejected]".into(),
                }
            } else {
                "[suggested post approved]".into()
            }
        }
        MessageAction::SuggestedPostSuccess(_) => "[suggested post published]".into(),
        MessageAction::SuggestedPostRefund(a) => {
            let who = if a.payer_initiated { " (payer initiated)" } else { "" };
            format!("[suggested post refund{who}]")
        }
        MessageAction::GiftTon(a) => {
            format!("[gift TON: {} {}, {} {}]", a.crypto_amount, a.crypto_currency, a.amount, a.currency)
        }
        MessageAction::SuggestBirthday(a) => {
            format!("[birthday suggested: {:?}]", a.birthday)
        }
        MessageAction::StarGiftPurchaseOffer(a) => {
            let status = if a.accepted { ", accepted" } else if a.declined { ", declined" } else { "" };
            format!("[star gift purchase offer{status}]")
        }
        MessageAction::StarGiftPurchaseOfferDeclined(a) => {
            let reason = if a.expired { " (expired)" } else { "" };
            format!("[star gift offer declined{reason}]")
        }
        MessageAction::NewCreatorPending(a) => {
            format!("[ownership transfer to {} pending]", a.new_creator_id)
        }
        MessageAction::ChangeCreator(a) => {
            format!("[ownership transferred to {}]", a.new_creator_id)
        }
    }
}

fn format_ids(ids: &[i64]) -> String {
    ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", ")
}
