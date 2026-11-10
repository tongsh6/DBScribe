//! Oracle Instant Client 的定位与初始化。
//!
//! 发布包把 Instant Client 放在程序同级的 `instantclient/` 目录（由打包脚本放入）；
//! 找不到该目录时退回系统搜索路径（PATH / LD_LIBRARY_PATH），方便已装客户端的用户。

use std::path::PathBuf;
use std::sync::Once;

static INIT: Once = Once::new();

/// 初始化 Oracle 客户端库（每个进程只执行一次）。
///
/// 程序目录下存在 Instant Client 时显式指定加载目录；否则直接返回，
/// 让 oracle crate 走默认搜索路径。
pub fn ensure_initialized() {
    INIT.call_once(|| {
        let Some(dir) = locate_client_dir() else {
            return;
        };
        let mut params = oracle::InitParams::new();
        if params.oracle_client_lib_dir(&dir).is_ok() {
            // 初始化失败不 panic：后续连接会再尝试默认搜索并给出明确错误
            let _ = params.init();
        }
    });
}

/// 在程序目录与应用数据目录下寻找 Instant Client。
///
/// 查找顺序：
/// 1. 程序旁的 `instantclient/`（开发期手动放置）
/// 2. 程序旁的 `resources/instantclient/`（打包附带）
/// 3. 应用数据目录下的 `instantclient/`（**懒安装**下载的位置）
fn locate_client_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(base) = exe.parent() {
            candidates.push(base.join("instantclient"));
            candidates.push(base.join("resources").join("instantclient"));
            candidates.push(base.join("lib").join("instantclient"));
        }
    }
    if let Some(data_dir) = app_data_dir() {
        candidates.push(data_dir.join("instantclient"));
    }

    for path in candidates {
        if !path.is_dir() {
            continue;
        }
        // 解压后常见结构是 instantclient/instantclient_23_26/oci.dll，需要下钻一层
        if let Some(nested) = find_nested_client_dir(&path) {
            return Some(nested);
        }
        return Some(path);
    }
    None
}

/// 当前是否已能找到 Instant Client（不下载）。
pub fn installed_client_dir() -> Option<PathBuf> {
    locate_client_dir()
}

/// 应用数据目录（与桌面端 store.json 所在位置一致）。
pub fn app_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("com.dbs.scribe"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|base| {
            PathBuf::from(base)
                .join("Library")
                .join("Application Support")
                .join("com.dbs.scribe")
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|base| PathBuf::from(base).join(".local/share"))
            })
            .map(|base| base.join("com.dbs.scribe"))
    }
}

/// 当前平台 Instant Client 的下载地址。
///
/// 只提供 zip 包的平台（Windows / Linux）；macOS 官方只发 dmg，
/// 需要手动安装，这里返回 None。
pub fn client_download_url() -> Option<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("https://download.oracle.com/otn_software/nt/instantclient/2326400/instantclient-basiclite-windows.x64-23.26.4.0.0.zip")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some("https://download.oracle.com/otn_software/linux/instantclient/2326400/instantclient-basiclite-linux.x64-23.26.4.0.0.zip")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some("https://download.oracle.com/otn_software/linux/instantclient/2326400/instantclient-basiclite-linux.arm64-23.26.4.0.0.zip")
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    {
        None
    }
}

/// 下载并解压 Instant Client 到应用数据目录（懒安装）。
///
/// 已安装时直接返回现有目录；`progress(已下载字节, 总字节)` 用于进度反馈，
/// 总字节未知时为 0。
pub fn ensure_client(progress: &mut dyn FnMut(u64, u64)) -> anyhow::Result<PathBuf> {
    if let Some(dir) = locate_client_dir() {
        return Ok(dir);
    }

    let url = client_download_url().ok_or_else(|| {
        anyhow::anyhow!(
            "当前平台暂不支持自动下载 Oracle 客户端（{}），\
             请手动安装 Instant Client 后放到应用数据目录或程序目录",
            client_lib_name()
        )
    })?;

    let base = app_data_dir().ok_or_else(|| anyhow::anyhow!("无法确定应用数据目录"))?;
    let target = base.join("instantclient");
    std::fs::create_dir_all(&target)?;

    let zip_path = target.join("instantclient.zip");
    download(url, &zip_path, progress)?;
    extract(&zip_path, &target)?;
    let _ = std::fs::remove_file(&zip_path);

    // zip 内通常还有一层 instantclient_23_26 目录
    if let Some(nested) = find_nested_client_dir(&target) {
        return Ok(nested);
    }
    if target.is_dir() {
        return Ok(target);
    }
    anyhow::bail!("Oracle 客户端解压后未找到 {}", client_lib_name())
}

fn download(
    url: &str,
    dest: &std::path::Path,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<()> {
    use std::io::{Read, Write};

    let mut resp = ureq::get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("下载 Oracle 客户端失败：{e}"))?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(dest)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    Ok(())
}

fn extract(zip_path: &std::path::Path, dest: &std::path::Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| anyhow::anyhow!("打开下载的客户端压缩包失败：{e}"))?;
    archive
        .extract(dest)
        .map_err(|e| anyhow::anyhow!("解压 Oracle 客户端失败：{e}"))?;
    Ok(())
}

fn find_nested_client_dir(base: &std::path::Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(base).ok()?;
    entries.flatten().map(|entry| entry.path()).find(|path| {
        path.is_dir()
            && path
                .file_name()
                .map(|name| name.to_string_lossy().starts_with("instantclient"))
                .unwrap_or(false)
    })
}

/// 当前平台的客户端主库文件名（用于文档与提示）。
pub fn client_lib_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "oci.dll"
    } else if cfg!(target_os = "macos") {
        "libclntsh.dylib"
    } else {
        "libclntsh.so"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_lib_name_matches_platform() {
        let name = client_lib_name();
        assert!(!name.is_empty());
        assert!(name.contains("oci") || name.contains("clntsh"));
    }
}
