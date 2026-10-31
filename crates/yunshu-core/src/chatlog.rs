//! 局域网聊天落库（SQLite）：飞鸽单聊 / 广播 / 手机文本，收发两侧都记。
//!
//! 库文件 chat.db 放配置目录（与密钥/信任表同处），WAL 模式支持多实例
//! （同机双开）跨进程并发读写；写入失败只告警，绝不阻断聊天本身。

use std::path::Path;
use std::sync::Mutex;

/// 聊天通道：一张表按 channel 列区分三类会话
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatChannel {
    /// 飞鸽 PC↔PC 单聊
    Peer,
    /// 飞鸽广播（"所有人"会话）
    Broadcast,
    /// 手机 ↔ PC（扫码配对）
    Phone,
}

impl ChatChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatChannel::Peer => "pigeon",
            ChatChannel::Broadcast => "broadcast",
            ChatChannel::Phone => "phone",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pigeon" => Some(ChatChannel::Peer),
            "broadcast" => Some(ChatChannel::Broadcast),
            "phone" => Some(ChatChannel::Phone),
            _ => None,
        }
    }
}

/// 消息方向（相对本机）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatDirection {
    In,
    Out,
}

impl ChatDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatDirection::In => "in",
            ChatDirection::Out => "out",
        }
    }
}

/// 一条聊天记录（查询结果）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatEntry {
    pub id: i64,
    /// Unix 毫秒
    pub ts: i64,
    pub channel: String,
    pub peer_id: String,
    pub peer_name: String,
    pub direction: String,
    pub text: String,
}

const CREATE_SQL: &str = "
CREATE TABLE IF NOT EXISTS chat_messages (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    ts        INTEGER NOT NULL,
    channel   TEXT    NOT NULL,
    peer_id   TEXT    NOT NULL DEFAULT '',
    peer_name TEXT    NOT NULL DEFAULT '',
    direction TEXT    NOT NULL,
    text      TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chat_messages_channel_ts ON chat_messages(channel, ts);
";

pub struct ChatLog {
    conn: Mutex<Option<rusqlite::Connection>>,
}

impl ChatLog {
    /// 打开（不存在则建表）。文件打不开时退回内存库：聊天照常，仅不跨重启留存。
    pub fn open(path: &Path) -> ChatLog {
        match Self::open_conn(path) {
            Ok(conn) => ChatLog { conn: Mutex::new(Some(conn)) },
            Err(e) => {
                tracing::warn!("聊天记录库打开失败（{:?}）：{e}，本次运行不落盘", path);
                ChatLog { conn: Mutex::new(None) }
            }
        }
    }

    /// 只读查询用的独立连接（CLI 查历史；库不存在返回 None）
    pub fn open_readonly(path: &Path) -> Option<ChatLog> {
        if !path.exists() {
            return None;
        }
        Self::open_conn(path).ok().map(|conn| ChatLog { conn: Mutex::new(Some(conn)) })
    }

    fn open_conn(path: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
        let conn = rusqlite::Connection::open(path)?;
        // WAL：多实例（同机双开）跨进程并发；busy_timeout 兜住写写互斥的瞬时报错
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(CREATE_SQL)?;
        Ok(conn)
    }
    /// 记一条消息（ts 取当前时间）。失败仅告警。
    pub fn record(
        &self,
        channel: ChatChannel,
        direction: ChatDirection,
        peer_id: &str,
        peer_name: &str,
        text: &str,
    ) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.record_at(ts, channel, direction, peer_id, peer_name, text);
    }

    /// 指定时间戳记一条（测试用；生产入口是 record）
    pub fn record_at(
        &self,
        ts: i64,
        channel: ChatChannel,
        direction: ChatDirection,
        peer_id: &str,
        peer_name: &str,
        text: &str,
    ) {
        let Ok(guard) = self.conn.lock() else {
            return;
        };
        let Some(conn) = guard.as_ref() else {
            return; // 内存库退回模式也没建表，无从写入
        };
        if let Err(e) = conn.execute(
            "INSERT INTO chat_messages (ts, channel, peer_id, peer_name, direction, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                ts,
                channel.as_str(),
                peer_id,
                peer_name,
                direction.as_str(),
                text
            ],
        ) {
            tracing::warn!("聊天记录写入失败：{e}");
        }
    }

    /// 最近 limit 条（升序返回，直接可按时间展示）。channel 为 None 时查全部通道。
    pub fn recent(&self, channel: Option<ChatChannel>, limit: usize) -> Vec<ChatEntry> {
        let Ok(guard) = self.conn.lock() else {
            return Vec::new();
        };
        let Some(conn) = guard.as_ref() else {
            return Vec::new();
        };
        let sql: &str = match channel {
            Some(_) => {
                "SELECT id, ts, channel, peer_id, peer_name, direction, text
                 FROM chat_messages WHERE channel = ?1
                 ORDER BY ts DESC, id DESC LIMIT ?2"
            }
            None => {
                "SELECT id, ts, channel, peer_id, peer_name, direction, text
                 FROM chat_messages
                 ORDER BY ts DESC, id DESC LIMIT ?1"
            }
        };

        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("聊天记录查询失败：{e}");
                return Vec::new();
            }
        };
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ChatEntry> {
            Ok(ChatEntry {
                id: row.get(0)?,
                ts: row.get(1)?,
                channel: row.get(2)?,
                peer_id: row.get(3)?,
                peer_name: row.get(4)?,
                direction: row.get(5)?,
                text: row.get(6)?,
            })
        };
        let rows = match (channel, limit) {
            (Some(c), l) => stmt.query_map(
                rusqlite::params![c.as_str(), l as i64],
                map_row,
            ),
            (None, l) => stmt.query_map(rusqlite::params![l as i64], map_row),
        };
        let mut v: Vec<ChatEntry> = match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                tracing::warn!("聊天记录查询失败：{e}");
                Vec::new()
            }
        };
        v.reverse();
        v
    }
}

impl ChatEntry {
    pub fn channel_enum(&self) -> Option<ChatChannel> {
        ChatChannel::from_str(&self.channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内存库 + 建表（与文件库同一套 schema）
    fn mem_log() -> ChatLog {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(CREATE_SQL).unwrap();
        ChatLog { conn: Mutex::new(Some(conn)) }
    }

    #[test]
    fn record_and_recent_roundtrip() {
        let log = mem_log();
        log.record_at(
            1_000,
            ChatChannel::Peer,
            ChatDirection::In,
            "peer-1",
            "电脑A",
            "你好",
        );
        log.record_at(
            2_000,
            ChatChannel::Peer,
            ChatDirection::Out,
            "peer-1",
            "电脑A",
            "你也好",
        );
        log.record_at(
            3_000,
            ChatChannel::Broadcast,
            ChatDirection::In,
            "peer-2",
            "电脑B",
            "大家好",
        );

        let all = log.recent(None, 10);
        assert_eq!(all.len(), 3);
        // 升序：时间早的在前
        assert_eq!(all[0].text, "你好");
        assert_eq!(all[2].text, "大家好");
        assert_eq!(all[2].channel, "broadcast");
        assert_eq!(all[0].direction, "in");
        assert_eq!(all[1].direction, "out");

        let peer = log.recent(Some(ChatChannel::Peer), 10);
        assert_eq!(peer.len(), 2);
        assert!(peer.iter().all(|e| e.channel == "pigeon"));
    }

    #[test]
    fn recent_limit_keeps_latest() {
        let log = mem_log();
        for i in 0..10 {
            log.record_at(
                i * 1000,
                ChatChannel::Phone,
                ChatDirection::In,
                "phone",
                "手机",
                &format!("消息{i}"),
            );
        }
        let last3 = log.recent(Some(ChatChannel::Phone), 3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].text, "消息7");
        assert_eq!(last3[2].text, "消息9");
    }

    #[test]
    fn channel_str_roundtrip() {
        for c in [
            ChatChannel::Peer,
            ChatChannel::Broadcast,
            ChatChannel::Phone,
        ] {
            assert_eq!(ChatChannel::from_str(c.as_str()), Some(c));
        }
        assert_eq!(ChatChannel::from_str("other"), None);
    }

    #[test]
    fn fallback_log_never_panics() {
        // 打不开文件（内存库退回）时 record/recent 静默无效
        let log = ChatLog { conn: Mutex::new(None) };
        log.record(ChatChannel::Peer, ChatDirection::In, "p", "n", "t");
        assert!(log.recent(None, 10).is_empty());
    }

    #[test]
    fn file_db_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("yunshu_chatlog_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("chat.db");
        let _ = std::fs::remove_file(&db);

        {
            let log = ChatLog::open(&db);
            log.record(ChatChannel::Peer, ChatDirection::Out, "p1", "电脑A", "持久化");
        }
        let ro = ChatLog::open_readonly(&db).expect("库应存在");
        let rows = ro.recent(None, 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "持久化");
        assert_eq!(rows[0].channel_enum(), Some(ChatChannel::Peer));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
