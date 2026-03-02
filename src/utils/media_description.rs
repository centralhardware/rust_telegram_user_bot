use grammers_client::update::Message;
use grammers_tl_types as tl;

pub fn describe(message: &Message) -> Option<String> {
    let media = extract_media(message)?;
    Some(describe_media(media))
}

fn extract_media(message: &Message) -> Option<&tl::enums::MessageMedia> {
    // message.raw is tl::enums::Update; we need to get the inner tl::types::Message
    // and its media field. The grammers Message type exposes raw as pub.
    // We go through the grammers high-level API instead: check if media() returns Some,
    // and if so, we know there's media. But we need the raw enum.
    // The simplest reliable way: use the serde-serialized raw to find media,
    // but that's wasteful. Instead, let's use the fact that grammers exposes
    // the raw message via a method.
    //
    // Actually, grammers Message has a `raw` field of type tl::enums::Update,
    // and within certain Update variants there's a tl::enums::Message.
    // But extracting from all Update variants is impractical.
    //
    // Better approach: grammers provides message.media() -> Option<Media>.
    // Media is #[non_exhaustive] so we can't fully match it.
    // But we can also look at the raw message field directly via internal structure.
    //
    // Let's just use a small helper that extracts from known Update variants.
    extract_media_from_update(&message.raw)
}

fn extract_media_from_update(update: &tl::enums::Update) -> Option<&tl::enums::MessageMedia> {
    let msg = match update {
        tl::enums::Update::NewMessage(u) => &u.message,
        tl::enums::Update::NewChannelMessage(u) => &u.message,
        tl::enums::Update::EditMessage(u) => &u.message,
        tl::enums::Update::EditChannelMessage(u) => &u.message,
        tl::enums::Update::NewScheduledMessage(u) => &u.message,
        _ => return None,
    };
    match msg {
        tl::enums::Message::Message(m) => m.media.as_ref(),
        tl::enums::Message::Empty(_) | tl::enums::Message::Service(_) => None,
    }
}

fn describe_media(media: &tl::enums::MessageMedia) -> String {
    match media {
        tl::enums::MessageMedia::Empty => "[empty media]".into(),
        tl::enums::MessageMedia::Unsupported => "[unsupported media]".into(),
        tl::enums::MessageMedia::Photo(p) => {
            if p.spoiler {
                "[photo, spoiler]".into()
            } else {
                "[photo]".into()
            }
        }
        tl::enums::MessageMedia::Document(doc) => describe_document(doc),
        tl::enums::MessageMedia::Contact(c) => {
            let name = join_non_empty(" ", &[&c.first_name, &c.last_name]);
            if name.is_empty() {
                format!("[contact, {}]", c.phone_number)
            } else {
                format!("[contact, {}, {}]", name, c.phone_number)
            }
        }
        tl::enums::MessageMedia::Geo(g) => {
            match &g.geo {
                tl::enums::GeoPoint::Point(p) => format!("[location, {:.5}, {:.5}]", p.lat, p.long),
                tl::enums::GeoPoint::Empty => "[location]".into(),
            }
        }
        tl::enums::MessageMedia::GeoLive(g) => {
            match &g.geo {
                tl::enums::GeoPoint::Point(p) => format!("[live location, {:.5}, {:.5}]", p.lat, p.long),
                tl::enums::GeoPoint::Empty => "[live location]".into(),
            }
        }
        tl::enums::MessageMedia::Venue(v) => format!("[venue, {}]", v.title),
        tl::enums::MessageMedia::Poll(p) => {
            let tl::enums::Poll::Poll(poll) = &p.poll;
            let tl::enums::TextWithEntities::Entities(q) = &poll.question;
            if poll.quiz {
                format!("[quiz: {}]", q.text)
            } else {
                format!("[poll: {}]", q.text)
            }
        }
        tl::enums::MessageMedia::Dice(d) => format!("[{} = {}]", d.emoticon, d.value),
        tl::enums::MessageMedia::WebPage(_) => "[web page]".into(),
        tl::enums::MessageMedia::Game(g) => {
            let tl::enums::Game::Game(game) = &g.game;
            format!("[game, {}]", game.title)
        }
        tl::enums::MessageMedia::Invoice(inv) => {
            format!("[invoice, {}]", inv.title)
        }
        tl::enums::MessageMedia::Story(_) => "[story]".into(),
        tl::enums::MessageMedia::Giveaway(g) => {
            format!("[giveaway, {} winners]", g.quantity)
        }
        tl::enums::MessageMedia::GiveawayResults(_) => "[giveaway results]".into(),
        tl::enums::MessageMedia::PaidMedia(p) => {
            format!("[paid media, {} stars]", p.stars_amount)
        }
        tl::enums::MessageMedia::ToDo(_) => "[todo list]".into(),
        tl::enums::MessageMedia::VideoStream(_) => "[video stream]".into(),
    }
}

fn describe_document(media: &tl::types::MessageMediaDocument) -> String {
    let doc = match media.document.as_ref() {
        Some(tl::enums::Document::Document(d)) => d,
        Some(tl::enums::Document::Empty(_)) => return "[document]".into(),
        None => return "[document]".into(),
    };

    let mut is_voice = false;
    let mut is_round = false;
    let mut is_sticker = false;
    let mut sticker_emoji: Option<&str> = None;
    let mut audio_duration: Option<i32> = None;
    let mut video_duration: Option<f64> = None;
    let mut audio_title: Option<&str> = None;
    let mut audio_performer: Option<&str> = None;
    let mut is_gif = false;
    let mut filename: Option<&str> = None;

    for attr in &doc.attributes {
        match attr {
            tl::enums::DocumentAttribute::Audio(a) => {
                is_voice = a.voice;
                audio_duration = Some(a.duration);
                audio_title = a.title.as_deref();
                audio_performer = a.performer.as_deref();
            }
            tl::enums::DocumentAttribute::Video(v) => {
                is_round = v.round_message;
                is_gif = v.nosound;
                video_duration = Some(v.duration);
            }
            tl::enums::DocumentAttribute::Sticker(s) => {
                is_sticker = true;
                sticker_emoji = Some(&s.alt);
            }
            tl::enums::DocumentAttribute::Filename(f) => {
                filename = Some(&f.file_name);
            }
            tl::enums::DocumentAttribute::Animated
            | tl::enums::DocumentAttribute::HasStickers
            | tl::enums::DocumentAttribute::ImageSize(_)
            | tl::enums::DocumentAttribute::CustomEmoji(_) => {}
        }
    }

    if is_sticker {
        let emoji = sticker_emoji.unwrap_or("");
        return format!("[sticker {emoji}]");
    }

    if is_voice {
        return format!("[voice, {}]", format_duration_secs(audio_duration.unwrap_or(0)));
    }

    if is_round {
        return format!("[video message, {}]", format_duration_f64(video_duration.unwrap_or(0.0)));
    }

    if let Some(dur) = audio_duration {
        let mut parts = vec!["audio".to_string()];
        if let Some(performer) = audio_performer {
            if let Some(title) = audio_title {
                parts.push(format!("{performer} — {title}"));
            } else {
                parts.push(performer.to_string());
            }
        } else if let Some(title) = audio_title {
            parts.push(title.to_string());
        }
        parts.push(format_duration_secs(dur));
        return format!("[{}]", parts.join(", "));
    }

    if let Some(dur) = video_duration {
        if is_gif {
            return "[GIF]".into();
        }
        return format!("[video, {}]", format_duration_f64(dur));
    }

    if let Some(name) = filename {
        format!("[file, {name}]")
    } else {
        "[document]".into()
    }
}

fn format_duration_secs(seconds: i32) -> String {
    let mins = seconds / 60;
    let secs = seconds % 60;
    format!("{mins}:{secs:02}")
}

fn format_duration_f64(seconds: f64) -> String {
    let total = seconds.round() as i32;
    format_duration_secs(total)
}

fn join_non_empty(sep: &str, parts: &[&str]) -> String {
    parts.iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(sep)
}
