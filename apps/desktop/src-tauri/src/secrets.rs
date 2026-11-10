//! 密码存储：交给操作系统的凭据管理器，不写进本地配置文件。
//!
//! - Windows：凭据管理器（DPAPI 加密）
//! - macOS：钥匙串
//! - Linux：Secret Service
//!
//! 拿不到凭据后端时（例如无桌面会话的 Linux）只记录警告，
//! 应用仍可正常使用，只是需要手动输密码。

use anyhow::{Context, Result};

const SERVICE: &str = "com.dbs.scribe";

fn entry(id: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, id).context("访问系统凭据存储失败")
}

/// 把密码写入系统凭据存储。
pub fn save_password(id: &str, password: &str) -> Result<()> {
    entry(id)?
        .set_password(password)
        .context("写入系统凭据存储失败")
}

/// 读取密码；不存在或后端不可用时返回 `None`。
pub fn load_password(id: &str) -> Option<String> {
    match entry(id) {
        Ok(e) => e.get_password().ok(),
        Err(err) => {
            eprintln!("[DBScribe] 读取系统凭据失败：{err:#}");
            None
        }
    }
}

/// 删除密码（忽略不存在的情况）。
pub fn delete_password(id: &str) {
    if let Ok(e) = entry(id) {
        // 凭据不存在时会报错，这里直接忽略
        let _ = e.delete_credential();
    }
}
