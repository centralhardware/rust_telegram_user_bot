use clickhouse::{Client, Row};
use serde::Serialize;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::mpsc;

static CLICKHOUSE: LazyLock<Client> = LazyLock::new(|| {
    Client::default()
        .with_url(std::env::var("CLICKHOUSE_URL").expect("CLICKHOUSE_URL not set"))
        .with_user(std::env::var("CLICKHOUSE_USER").expect("CLICKHOUSE_USER not set"))
        .with_password(std::env::var("CLICKHOUSE_PASSWORD").expect("CLICKHOUSE_PASSWORD not set"))
        .with_database(std::env::var("CLICKHOUSE_DATABASE").expect("CLICKHOUSE_DATABASE not set"))
});

pub fn clickhouse() -> &'static Client {
    &CLICKHOUSE
}

pub struct WriteBuffer<T: Send + 'static> {
    tx: mpsc::UnboundedSender<T>,
}

impl<T> WriteBuffer<T>
where
    T: Serialize + Send + 'static,
    for<'a> T: Row<Value<'a> = T>,
{
    pub fn new(table: &'static str) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(Self::flush_loop(rx, table));
        Self { tx }
    }

    pub fn push(&self, row: T) {
        let _ = self.tx.send(row);
    }

    async fn flush_loop(mut rx: mpsc::UnboundedReceiver<T>, table: &str) {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let mut buffer: Vec<T> = Vec::new();
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if !buffer.is_empty() {
                        Self::flush(&mut buffer, table).await;
                    }
                }
                item = rx.recv() => {
                    match item {
                        Some(row) => buffer.push(row),
                        None => {
                            if !buffer.is_empty() {
                                Self::flush(&mut buffer, table).await;
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn flush(buffer: &mut Vec<T>, table: &str) {
        let count = buffer.len();
        match clickhouse().insert::<T>(table).await {
            Ok(mut insert) => {
                for row in buffer.drain(..) {
                    if let Err(e) = insert.write(&row).await {
                        log::error!("buffer write to {table}: {e}");
                        return;
                    }
                }
                if let Err(e) = insert.end().await {
                    log::error!("buffer flush to {table}: {e}");
                } else {
                    log::info!("flushed {count} rows to {table}");
                }
            }
            Err(e) => log::error!("buffer insert to {table}: {e}"),
        }
    }
}

static INCOMING_BUF: LazyLock<WriteBuffer<IncomingMessage>> =
    LazyLock::new(|| WriteBuffer::new("chats_log"));
static EDITED_BUF: LazyLock<WriteBuffer<EditedMessage>> =
    LazyLock::new(|| WriteBuffer::new("edited_log"));
static DELETED_BUF: LazyLock<WriteBuffer<DeletedMessage>> =
    LazyLock::new(|| WriteBuffer::new("deleted_log"));

pub fn incoming_buffer() -> &'static WriteBuffer<IncomingMessage> { &INCOMING_BUF }
pub fn edited_buffer() -> &'static WriteBuffer<EditedMessage> { &EDITED_BUF }
pub fn deleted_buffer() -> &'static WriteBuffer<DeletedMessage> { &DELETED_BUF }

#[derive(Row, Serialize)]
pub struct IncomingMessage {
    pub date_time: u32,
    pub message: String,
    pub chat_title: String,
    pub chat_id: i64,
    pub username: Vec<String>,
    pub first_name: String,
    pub second_name: String,
    pub user_id: u64,
    pub message_id: i64,
    pub chat_usernames: Vec<String>,
    pub reply_to: u64,
    pub client_id: u64,
}

#[derive(Row, Serialize)]
pub struct OutgoingMessage {
    pub date_time: u32,
    pub message: String,
    pub title: String,
    pub id: i64,
    pub admins2: Vec<String>,
    pub usernames: Vec<String>,
    pub message_id: u64,
    pub reply_to: u64,
    pub raw: String,
    pub client_id: u64,
}

#[derive(Row, Serialize)]
pub struct EditedMessage {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    pub original_message: String,
    pub message: String,
    pub diff: String,
    pub user_id: i64,
    pub client_id: u64,
}

#[derive(Row, Serialize)]
pub struct DeletedMessage {
    pub date_time: u32,
    pub chat_id: i64,
    pub message_id: i64,
    pub client_id: u64,
}

#[derive(Row, Serialize)]
pub struct AdminAction {
    pub date: u32,
    pub event_id: u64,
    pub chat_id: u64,
    pub action_type: String,
    pub user_id: u64,
    pub message: String,
    pub log_output: String,
    pub usernames: Vec<String>,
    pub chat_usernames: Vec<String>,
    pub chat_title: String,
    pub user_title: String,
}

#[derive(Row, Serialize)]
pub struct TelegramSession {
    pub hash: i64,
    pub device_model: String,
    pub platform: String,
    pub system_version: Option<String>,
    pub app_name: String,
    pub app_version: Option<String>,
    pub ip: Option<String>,
    pub country: String,
    pub region: String,
    pub date_created: u32,
    pub date_active: u32,
    pub updated_at: u32,
    pub client_id: u64,
}
