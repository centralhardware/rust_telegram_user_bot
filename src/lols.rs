//! Whether the sender of an incoming message is on the lols.bot spam banlist
//! (<https://lols.bot>). Asked before the message is saved; the answer is
//! the row's `sender_spam` column (migration 054).
//!
//! [`Lols::is_spam`] answers from, in order:
//!
//! 1. The hourly dump (`banlist.txt`, one id per line), held in memory as a
//!    sorted `Vec` and downloaded again whenever its ETag changes.
//! 2. The cache of earlier API answers: a spammer for the life of the process,
//!    a clean user for [`CLEAN_FOR`].
//! 3. `api.lols.bot/account`, given [`API_TIMEOUT`] at most, since the save is
//!    waiting. A failure or a timeout answers "not spam" and is not cached.
//!
//! One checker serves every account: it is about users, not about which
//! account saw them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Result, bail};
use log::{info, warn};
use serde::Deserialize;

use crate::db::now;

const DUMP_URL: &str = "https://lols.bot/spam/banlist.txt";
const API_URL: &str = "https://api.lols.bot/account";
/// The dump is rebuilt hourly.
const DUMP_EVERY: Duration = Duration::from_secs(3600);
/// How long a save may wait on the API.
const API_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a clean API answer is trusted. A spammer is cached for good.
const CLEAN_FOR: u32 = 24 * 3600;
/// The cache starts over past this, the way `WrittenNames` does.
const CACHE_LIMIT: usize = 1_000_000;

/// `api.lols.bot/account`'s answer, only the fields used here.
#[derive(Deserialize, Debug)]
struct Account {
    ok: bool,
    banned: bool,
}

/// The banlist dump: user ids, sorted for binary search. Channel ids
/// (`-100…`) in it are kept; they never match a user.
#[derive(Default)]
struct Dump {
    ids: Vec<i64>,
    etag: Option<String>,
}

impl Dump {
    fn contains(&self, id: i64) -> bool {
        self.ids.binary_search(&id).is_ok()
    }
}

fn parse_dump(text: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// A cached API answer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cached {
    Spammer,
    /// Clean as of this time.
    Clean(u32),
}

/// What the cache says about a user, `None` when it has to be asked.
fn cached_answer(entry: Option<Cached>, at: u32) -> Option<bool> {
    match entry? {
        Cached::Spammer => Some(true),
        Cached::Clean(since) if at.saturating_sub(since) < CLEAN_FOR => Some(false),
        Cached::Clean(_) => None,
    }
}

pub struct Lols {
    /// Off in tests: every answer is "not spam" and nothing goes out.
    enabled: bool,
    http: reqwest::Client,
    dump: RwLock<Dump>,
    cache: Mutex<HashMap<i64, Cached>>,
}

impl Lols {
    /// A checker that does nothing: what an `App` starts with, and what tests
    /// run on.
    pub fn disabled() -> Self {
        Self::new(false)
    }

    fn new(enabled: bool) -> Self {
        Lols {
            enabled,
            http: reqwest::Client::builder()
                .user_agent("rust_telegram_user_bot")
                .timeout(Duration::from_secs(120))
                .build()
                .expect("http client"),
            dump: RwLock::default(),
            cache: Mutex::default(),
        }
    }

    /// The checker, with the dump kept fresh in the background from now on.
    pub fn start() -> Arc<Self> {
        let lols = Arc::new(Self::new(true));
        let refresher = Arc::clone(&lols);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(DUMP_EVERY);
            loop {
                interval.tick().await;
                if let Err(e) = refresher.refresh_dump().await {
                    warn!("lols: banlist download failed, keeping the previous one: {e:#}");
                }
            }
        });
        lols
    }

    /// Whether `user_id` is on the banlist. 0 is no user (a channel post, an
    /// anonymous admin) and is never spam.
    pub async fn is_spam(&self, user_id: u64) -> bool {
        let Ok(user_id) = i64::try_from(user_id) else {
            return false;
        };
        if !self.enabled || user_id <= 0 {
            return false;
        }
        if self.dump.read().unwrap().contains(user_id) {
            return true;
        }
        let at = now();
        if let Some(answer) = cached_answer(self.cache.lock().unwrap().get(&user_id).copied(), at) {
            return answer;
        }
        match check(&self.http, user_id).await {
            Ok(banned) => {
                let mut cache = self.cache.lock().unwrap();
                if cache.len() >= CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(
                    user_id,
                    if banned {
                        Cached::Spammer
                    } else {
                        Cached::Clean(at)
                    },
                );
                if banned {
                    info!("lols: {user_id} is on the spam banlist (api)");
                }
                banned
            }
            Err(e) => {
                warn!("lols: API check of {user_id}: {e:#}");
                false
            }
        }
    }

    /// Downloads the dump when it has changed.
    async fn refresh_dump(&self) -> Result<()> {
        let mut req = self.http.get(DUMP_URL);
        if let Some(etag) = self.dump.read().unwrap().etag.clone() {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(());
        }
        let resp = resp.error_for_status()?;
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let ids = parse_dump(&resp.text().await?);
        if ids.is_empty() {
            bail!("banlist is empty");
        }
        info!("lols: banlist loaded, {} ids", ids.len());
        *self.dump.write().unwrap() = Dump { ids, etag };
        Ok(())
    }
}

async fn check(http: &reqwest::Client, user_id: i64) -> Result<bool> {
    let account: Account = http
        .get(API_URL)
        .query(&[("id", user_id)])
        .timeout(API_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if !account.ok {
        bail!("api answered ok=false");
    }
    Ok(account.banned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dump_parses_and_skips_junk() {
        let ids = parse_dump("5\n-1001\n\n x\n3\n5\n");
        assert_eq!(ids, vec![-1001, 3, 5]);
        let dump = Dump { ids, etag: None };
        assert!(dump.contains(3));
        assert!(!dump.contains(4));
    }

    #[test]
    fn what_the_cache_answers() {
        let at = 10_000_000;
        assert_eq!(cached_answer(None, at), None);
        assert_eq!(cached_answer(Some(Cached::Spammer), at), Some(true));
        assert_eq!(cached_answer(Some(Cached::Clean(at - 60)), at), Some(false));
        assert_eq!(cached_answer(Some(Cached::Clean(at - CLEAN_FOR)), at), None);
    }

    #[tokio::test]
    async fn a_disabled_checker_says_not_spam() {
        assert!(!Lols::disabled().is_spam(42).await);
    }

    #[test]
    fn the_api_answer_parses() {
        let a: Account = serde_json::from_str(
            r#"{"ok":true,"user_id":7,"banned":true,"when":"2024-06-28 17:18:05 UTC","offenses":3}"#,
        )
        .unwrap();
        assert!(a.ok && a.banned);
        let clean: Account =
            serde_json::from_str(r#"{"ok":true,"user_id":7,"banned":false}"#).unwrap();
        assert!(!clean.banned);
    }
}
