use grammers_client::message::Message;
use grammers_tl_types as tl;

/// Render the `rich_message` payload of a message (layer 228+) into markdown-like text.
/// Rich messages carry their content as Instant-View `PageBlock`s instead of
/// `message` + `entities`, so without this the log would only see an empty body.
pub fn rich_text(message: &Message) -> Option<String> {
    render(extract_rich(&message.raw)?)
}

/// The same, for a payload that did not arrive on a `Message` — an ephemeral
/// message carries its own `rich_message`.
pub fn render(rich: &tl::enums::RichMessage) -> Option<String> {
    let tl::enums::RichMessage::Message(rich) = rich;

    let mut out = render_blocks(&rich.blocks);
    if rich.part {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("[rich message truncated]");
    }
    if out.is_empty() { None } else { Some(out) }
}

fn extract_rich(msg: &tl::enums::Message) -> Option<&tl::enums::RichMessage> {
    match msg {
        tl::enums::Message::Message(m) => m.rich_message.as_ref(),
        tl::enums::Message::Empty(_) | tl::enums::Message::Service(_) => None,
    }
}

fn render_blocks(blocks: &[tl::enums::PageBlock]) -> String {
    blocks
        .iter()
        .map(render_block)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_block(block: &tl::enums::PageBlock) -> String {
    use tl::enums::PageBlock as B;
    match block {
        B::Unsupported => "[unsupported block]".into(),
        B::Title(b) => heading(1, &b.text),
        B::Subtitle(b) => heading(2, &b.text),
        B::Header(b) => heading(2, &b.text),
        B::Subheader(b) => heading(3, &b.text),
        B::Heading1(b) => heading(1, &b.text),
        B::Heading2(b) => heading(2, &b.text),
        B::Heading3(b) => heading(3, &b.text),
        B::Heading4(b) => heading(4, &b.text),
        B::Heading5(b) => heading(5, &b.text),
        B::Heading6(b) => heading(6, &b.text),
        B::Kicker(b) => render_text(&b.text),
        B::Paragraph(b) => render_text(&b.text),
        B::Footer(b) => render_text(&b.text),
        B::AuthorDate(b) => {
            let author = render_text(&b.author);
            match (author.is_empty(), b.published_date) {
                (true, 0) => String::new(),
                (true, d) => format_date(d),
                (false, 0) => author,
                (false, d) => format!("{}, {}", author, format_date(d)),
            }
        }
        B::Preformatted(b) => {
            let lang = &b.language;
            format!("```{}\n{}\n```", lang, render_text(&b.text))
        }
        B::Math(b) => format!("$$\n{}\n$$", b.source),
        B::Thinking(b) => quote(&format!("💭 {}", render_text(&b.text))),
        B::Divider => "---".into(),
        B::Anchor(_) => String::new(),
        B::List(b) => b
            .items
            .iter()
            .map(|item| {
                let (mark, body) = match item {
                    tl::enums::PageListItem::Text(i) => {
                        (checkbox(i.checkbox, i.checked), render_text(&i.text))
                    }
                    tl::enums::PageListItem::Blocks(i) => {
                        (checkbox(i.checkbox, i.checked), render_blocks(&i.blocks))
                    }
                };
                bullet(&format!("-{} ", mark), &body)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        B::OrderedList(b) => {
            let mut counter = b.start.unwrap_or(1);
            b.items
                .iter()
                .map(|item| {
                    let (num, value, check, body) = match item {
                        tl::enums::PageListOrderedItem::Text(i) => (
                            i.num.as_deref(),
                            i.value,
                            checkbox(i.checkbox, i.checked),
                            render_text(&i.text),
                        ),
                        tl::enums::PageListOrderedItem::Blocks(i) => (
                            i.num.as_deref(),
                            i.value,
                            checkbox(i.checkbox, i.checked),
                            render_blocks(&i.blocks),
                        ),
                    };
                    let label = match (num, value) {
                        (Some(n), _) => n.to_string(),
                        (None, Some(v)) => v.to_string(),
                        (None, None) => {
                            let n = counter;
                            counter += 1;
                            n.to_string()
                        }
                    };
                    bullet(&format!("{}.{} ", label, check), &body)
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        B::Blockquote(b) => with_caption(quote(&render_text(&b.text)), &render_text(&b.caption)),
        B::Pullquote(b) => with_caption(quote(&render_text(&b.text)), &render_text(&b.caption)),
        B::BlockquoteBlocks(b) => {
            with_caption(quote(&render_blocks(&b.blocks)), &render_text(&b.caption))
        }
        B::Details(b) => {
            let title = render_text(&b.title);
            let marker = if b.open { "▾" } else { "▸" };
            let body = indent(&render_blocks(&b.blocks), "  ");
            if body.is_empty() {
                format!("{} {}", marker, title)
            } else {
                format!("{} {}\n{}", marker, title, body)
            }
        }
        B::Photo(b) => {
            let label = if b.spoiler { "[photo, spoiler]" } else { "[photo]" };
            let label = match &b.url {
                Some(url) => format!("{}({})", label, url),
                None => label.to_string(),
            };
            with_page_caption(label, &b.caption)
        }
        B::Video(b) => {
            let label = if b.spoiler { "[video, spoiler]" } else { "[video]" };
            with_page_caption(label.to_string(), &b.caption)
        }
        B::Audio(b) => with_page_caption("[audio]".into(), &b.caption),
        B::Map(b) => with_page_caption("[map]".into(), &b.caption),
        B::InputPageBlockMap(b) => with_page_caption("[map]".into(), &b.caption),
        B::Cover(b) => render_block(&b.cover),
        B::Embed(b) => {
            let label = match &b.url {
                Some(url) => format!("[embed]({})", url),
                None => "[embed]".to_string(),
            };
            with_page_caption(label, &b.caption)
        }
        B::EmbedPost(b) => {
            let head = format!("[post by {}]({})", b.author, b.url);
            let body = render_blocks(&b.blocks);
            let joined = if body.is_empty() {
                head
            } else {
                format!("{}\n{}", head, indent(&body, "  "))
            };
            with_page_caption(joined, &b.caption)
        }
        B::Collage(b) => with_page_caption(render_blocks(&b.items), &b.caption),
        B::Slideshow(b) => with_page_caption(render_blocks(&b.items), &b.caption),
        B::Channel(b) => {
            let title = match &b.channel {
                tl::enums::Chat::Chat(c) => c.title.clone(),
                tl::enums::Chat::Channel(c) => c.title.clone(),
                tl::enums::Chat::Forbidden(c) => c.title.clone(),
                tl::enums::Chat::ChannelForbidden(c) => c.title.clone(),
                tl::enums::Chat::Community(c) => c.title.clone(),
                tl::enums::Chat::CommunityForbidden(c) => c.title.clone(),
                tl::enums::Chat::Empty(_) => String::new(),
            };
            format!("[channel: {}]", title)
        }
        B::RelatedArticles(b) => {
            let title = render_text(&b.title);
            let articles = b
                .articles
                .iter()
                .map(|a| {
                    let tl::enums::PageRelatedArticle::Article(a) = a;
                    let name = a.title.clone().unwrap_or_else(|| a.url.clone());
                    format!("- [{}]({})", name, a.url)
                })
                .collect::<Vec<_>>()
                .join("\n");
            if title.is_empty() { articles } else { format!("{}\n{}", title, articles) }
        }
        B::ButtonRow(b) => b
            .buttons
            .iter()
            .map(|button| {
                let tl::enums::PageButton::Button(button) = button;
                render_button(&button.text, &button.r#type)
            })
            .collect::<Vec<_>>()
            .join(" "),
        B::Document(b) => with_page_caption("[document]".into(), &b.caption),
        B::Table(b) => render_table(b),
    }
}

fn render_table(table: &tl::types::PageBlockTable) -> String {
    let rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| {
            let tl::enums::PageTableRow::Row(row) = row;
            row.cells
                .iter()
                .map(|cell| {
                    let tl::enums::PageTableCell::Cell(cell) = cell;
                    cell.text
                        .as_ref()
                        .map(render_text)
                        .unwrap_or_default()
                        .replace('\n', " ")
                        .replace('|', "\\|")
                })
                .collect()
        })
        .collect();

    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if width == 0 {
        return render_text(&table.title);
    }

    let line = |cells: &[String]| {
        let mut cells = cells.to_vec();
        cells.resize(width, String::new());
        format!("| {} |", cells.join(" | "))
    };

    let mut out = Vec::new();
    let title = render_text(&table.title);
    if !title.is_empty() {
        out.push(title);
    }
    out.push(line(&rows[0]));
    out.push(format!("|{}", " --- |".repeat(width)));
    for row in &rows[1..] {
        out.push(line(row));
    }
    out.join("\n")
}

fn render_text(text: &tl::enums::RichText) -> String {
    use tl::enums::RichText as T;
    match text {
        T::TextEmpty => String::new(),
        T::TextPlain(t) => t.text.clone(),
        T::TextConcat(t) => t.texts.iter().map(render_text).collect(),
        T::TextBold(t) => wrap(&render_text(&t.text), "**", "**"),
        T::TextItalic(t) => wrap(&render_text(&t.text), "*", "*"),
        T::TextUnderline(t) => wrap(&render_text(&t.text), "__", "__"),
        T::TextStrike(t) => wrap(&render_text(&t.text), "~~", "~~"),
        T::TextSpoiler(t) => wrap(&render_text(&t.text), "||", "||"),
        T::TextMarked(t) => wrap(&render_text(&t.text), "==", "=="),
        T::TextFixed(t) => wrap(&render_text(&t.text), "`", "`"),
        T::TextUrl(t) => link(&render_text(&t.text), &t.url),
        T::TextEmail(t) => link(&render_text(&t.text), &format!("mailto:{}", t.email)),
        T::TextPhone(t) => link(&render_text(&t.text), &format!("tel:{}", t.phone)),
        T::TextMath(t) => format!("${}$", t.source),
        T::TextImage(_) => "[image]".into(),
        T::TextCustomEmoji(t) => t.alt.clone(),
        T::TextAnchor(t) => render_text(&t.text),
        T::TextSubscript(t) => render_text(&t.text),
        T::TextSuperscript(t) => render_text(&t.text),
        T::TextMention(t) => render_text(&t.text),
        T::TextMentionName(t) => render_text(&t.text),
        T::TextHashtag(t) => render_text(&t.text),
        T::TextCashtag(t) => render_text(&t.text),
        T::TextBotCommand(t) => render_text(&t.text),
        T::TextAutoUrl(t) => render_text(&t.text),
        T::TextAutoEmail(t) => render_text(&t.text),
        T::TextAutoPhone(t) => render_text(&t.text),
        T::TextBankCard(t) => render_text(&t.text),
        T::TextDate(t) => render_text(&t.text),
        T::TextDiff(t) => render_text(&t.text),
        T::TextButton(t) => render_button(&t.text, &t.r#type),
    }
}

fn render_button(text: &tl::enums::RichText, kind: &tl::enums::InlineButtonType) -> String {
    use tl::enums::InlineButtonType as T;
    let label = render_text(text);
    match kind {
        T::Url(t) => link(&label, &t.url),
        T::UrlAuth(t) => link(&label, &t.url),
        T::InputInlineButtonTypeUrlAuth(t) => link(&label, &t.url),
        T::WebView(t) => link(&label, &t.url),
        _ => format!("[{}]", label),
    }
}

fn wrap(text: &str, open: &str, close: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("{}{}{}", open, text, close)
    }
}

fn link(text: &str, url: &str) -> String {
    if text.is_empty() {
        url.to_string()
    } else {
        format!("[{}]({})", text, url)
    }
}

fn heading(level: usize, text: &tl::enums::RichText) -> String {
    let text = render_text(text);
    if text.is_empty() {
        String::new()
    } else {
        format!("{} {}", "#".repeat(level), text)
    }
}

fn checkbox(checkbox: bool, checked: bool) -> &'static str {
    match (checkbox, checked) {
        (false, _) => "",
        (true, true) => " [x]",
        (true, false) => " [ ]",
    }
}

/// First line gets `mark`, continuation lines align under it.
fn bullet(mark: &str, body: &str) -> String {
    let pad = " ".repeat(mark.chars().count());
    let mut lines = body.lines();
    let first = lines.next().unwrap_or_default();
    let rest: Vec<String> = lines.map(|l| format!("{}{}", pad, l)).collect();
    let mut out = format!("{}{}", mark, first);
    for line in rest {
        out.push('\n');
        out.push_str(&line);
    }
    out
}

fn indent(text: &str, prefix: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    text.lines()
        .map(|l| format!("{}{}", prefix, l))
        .collect::<Vec<_>>()
        .join("\n")
}

fn quote(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    text.lines()
        .map(|l| if l.is_empty() { ">".to_string() } else { format!("> {}", l) })
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_caption(body: String, caption: &str) -> String {
    match (body.is_empty(), caption.is_empty()) {
        (_, true) => body,
        (true, false) => caption.to_string(),
        (false, false) => format!("{}\n{}", body, caption),
    }
}

fn with_page_caption(body: String, caption: &tl::enums::PageCaption) -> String {
    let tl::enums::PageCaption::Caption(caption) = caption;
    let text = render_text(&caption.text);
    let credit = render_text(&caption.credit);
    let caption = match (text.is_empty(), credit.is_empty()) {
        (true, true) => String::new(),
        (false, true) => text,
        (true, false) => credit,
        (false, false) => format!("{} — {}", text, credit),
    };
    with_caption(body, &caption)
}

fn format_date(ts: i32) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

