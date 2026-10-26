//! 配对状态机：token 校验 → pending（6 位码）→ active（session）。

use std::time::{Duration, Instant};

/// 二维码 token 的有效期：轮换周期 60s，旧 token 保留 5 分钟，
/// 避免用户扫码后停留期间 token 刷新导致失效。
pub const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);
/// 配对码有效期
pub const PENDING_TTL: Duration = Duration::from_secs(60);
/// 配对码最大错误次数
pub const MAX_ATTEMPTS: u32 = 5;

#[derive(Debug)]
pub struct Pending {
    pub device: String,
    pub code: String,
    pub created_at: Instant,
    pub attempts: u32,
}

#[derive(Debug, Default)]
pub struct Pairing {
    tokens: Vec<(String, Instant)>,
    pending: Option<Pending>,
    session: Option<String>,
}

impl Pairing {
    pub fn add_token(&mut self, token: String) {
        self.tokens.retain(|(_, t)| t.elapsed() < TOKEN_TTL);
        self.tokens.push((token, Instant::now()));
    }

    pub fn token_valid(&self, token: &str) -> bool {
        self.tokens.iter().any(|(t, _)| t == token)
    }

    /// 有设备发起配对，生成配对码。
    /// 已有未过期的待配对时只更新设备名、保留原码——手机端改名字重发 hello
    /// 不应导致用户正在输入的配对码失效。
    pub fn begin_pending(&mut self, device: String) -> String {
        if let Some(p) = self.pending.as_mut()
            && p.created_at.elapsed() <= PENDING_TTL {
                p.device = device;
                return p.code.clone();
            }
        let code = format!("{:06}", rand::random::<u32>() % 900_000 + 100_000);
        self.pending = Some(Pending { device, code: code.clone(), created_at: Instant::now(), attempts: 0 });
        code
    }

    /// 校验配对码，成功返回 (session, 设备名)
    pub fn verify(&mut self, code: &str) -> Result<(String, String), &'static str> {
        let Some(p) = self.pending.as_mut() else {
            return Err("当前没有待确认的配对，请重新扫码");
        };
        if p.created_at.elapsed() > PENDING_TTL {
            self.pending = None;
            return Err("配对已超时，请重新扫码");
        }
        if p.attempts >= MAX_ATTEMPTS {
            self.pending = None;
            return Err("配对码错误次数过多，请重新扫码");
        }
        p.attempts += 1;
        if p.code != code {
            return Err("配对码不正确");
        }
        let device = p.device.clone();
        self.pending = None;
        let session = uuid::Uuid::new_v4().simple().to_string();
        self.session = Some(session.clone());
        Ok((session, device))
    }

    pub fn session_valid(&self, session: &str) -> bool {
        self.session.as_deref() == Some(session)
    }

    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// 连接断开或电脑端主动断开时重置
    pub fn reset(&mut self) {
        self.pending = None;
        self.session = None;
    }
}
