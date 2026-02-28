use clickhouse::{Client, Row};
use serde::Serialize;
use std::sync::LazyLock;
use tokio::sync::Mutex;

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
    table: &'static str,
    buffer: Mutex<Vec<T>>,
}

impl<T> WriteBuffer<T>
where
    T: Serialize + Send + 'static,
    for<'a> T: Row<Value<'a> = T>,
{
    pub const fn new(table: &'static str) -> Self {
        Self {
            table,
            buffer: Mutex::const_new(Vec::new()),
        }
    }

    pub async fn push(&self, row: T) {
        self.buffer.lock().await.push(row);
    }

    pub async fn flush(&self) -> usize {
        let rows: Vec<T> = {
            let mut buf = self.buffer.lock().await;
            if buf.is_empty() {
                return 0;
            }
            std::mem::take(&mut *buf)
        };
        let count = rows.len();
        match clickhouse().insert::<T>(self.table).await {
            Ok(mut insert) => {
                for row in rows {
                    if let Err(e) = insert.write(&row).await {
                        log::error!("buffer write to {}: {e}", self.table);
                        return 0;
                    }
                }
                if let Err(e) = insert.end().await {
                    log::error!("buffer flush to {}: {e}", self.table);
                    0
                } else {
                    count
                }
            }
            Err(e) => {
                log::error!("buffer insert to {}: {e}", self.table);
                0
            }
        }
    }
}

pub static INCOMING_BUF: WriteBuffer<IncomingMessage> = WriteBuffer::new("chats_log");
pub static EDITED_BUF: WriteBuffer<EditedMessage> = WriteBuffer::new("edited_log");
pub static DELETED_BUF: WriteBuffer<DeletedMessage> = WriteBuffer::new("deleted_log");

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
