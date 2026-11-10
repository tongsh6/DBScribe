//! 达梦 ODBC 驱动检测与安装引导。
//!
//! 达梦没有纯 Rust 驱动，只能走系统 ODBC。这里负责：
//! 1. 检测系统是否注册了达梦驱动；
//! 2. 未安装时给出分平台的安装指引，供 CLI 报错与 GUI 引导对话框使用。

/// 驱动检测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverStatus {
    /// 已注册达梦 ODBC 驱动。
    Available { description: String },
    /// 未检测到驱动（ODBC 环境本身可用）。
    Missing,
    /// 当前平台不支持（macOS）。
    UnsupportedPlatform,
}

impl DriverStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, DriverStatus::Available { .. })
    }
}

/// 检测达梦 ODBC 驱动。
pub fn detect() -> DriverStatus {
    #[cfg(target_os = "macos")]
    {
        DriverStatus::UnsupportedPlatform
    }
    #[cfg(not(target_os = "macos"))]
    {
        match find_dameng_driver() {
            Some(description) => DriverStatus::Available { description },
            None => DriverStatus::Missing,
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn find_dameng_driver() -> Option<String> {
    use odbc_api::Environment;

    let env = Environment::new().ok()?;
    let drivers = env.drivers().ok()?;

    drivers.iter().find_map(|attribute| {
        let description = attribute.description.clone();
        let lower = description.to_lowercase();
        // 达梦驱动描述通常是 "DM ODBC DRIVER" / "Dameng DM8 ODBC Driver" / "DM8 ODBC DRIVER"
        let is_dameng = lower.contains("dameng")
            || lower.contains("dm8")
            || lower.contains("dm odbc")
            || lower.starts_with("dm ");
        is_dameng.then_some(description)
    })
}

/// 未安装驱动时的安装指引（按平台给出具体命令）。
pub fn install_guide() -> &'static str {
    if cfg!(target_os = "windows") {
        "未检测到达梦 ODBC 驱动。\n\n\
         使用达梦需要先安装 DM ODBC 驱动：\n\
         1. 安装达梦数据库客户端（安装时勾选「ODBC 驱动」组件）；\n\
         2. 或从达梦官网单独下载对应版本的 DM8 ODBC 驱动安装包。\n\n\
         安装完成后回到这里点「重新检测」即可。"
    } else {
        "未检测到达梦 ODBC 驱动。\n\n\
         使用达梦需要先安装 unixODBC 与达梦驱动：\n\
         1. 安装 unixODBC：sudo apt install unixodbc（或 yum install unixODBC）；\n\
         2. 安装达梦客户端，其 ODBC 驱动会注册到 /etc/odbcinst.ini；\n\
         3. 用 odbcinst -q -d 确认能看到达梦驱动。\n\n\
         安装完成后回到这里点「重新检测」即可。"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_a_status_without_panicking() {
        // 不依赖真实环境：只保证检测逻辑本身可运行、不 panic
        let status = detect();
        match status {
            DriverStatus::Available { .. } | DriverStatus::Missing => {}
            // UnsupportedPlatform 只在 macOS 下出现（编译期即确定）
            DriverStatus::UnsupportedPlatform => {}
        }
    }

    #[test]
    fn install_guide_mentions_odbc_driver() {
        let guide = install_guide();
        assert!(guide.contains("ODBC"));
        assert!(guide.contains("达梦"));
    }
}
