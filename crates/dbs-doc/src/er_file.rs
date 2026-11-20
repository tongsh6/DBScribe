//! ER 文件写入：先完整写入同目录临时文件，再替换目标文件。
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub struct StagedErFile {
    file: tempfile::NamedTempFile,
    destination: PathBuf,
}

impl StagedErFile {
    pub fn prepare(destination: &Path, data: &[u8]) -> Result<Self> {
        if destination.file_name().is_none() || destination.is_dir() {
            bail!("目标不是文件：{}", destination.display());
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(data)?;
        file.flush()?;
        Ok(Self {
            file,
            destination: destination.to_owned(),
        })
    }
    pub fn save(self) -> Result<()> {
        self.file
            .persist(&self.destination)
            .with_context(|| format!("替换文件失败：{}", self.destination.display()))?;
        Ok(())
    }

    /// GUI 为已有文件另取名称，原子发布时也不得覆盖并发创建的文件。
    pub fn save_new(self) -> Result<()> {
        self.file
            .persist_noclobber(&self.destination)
            .with_context(|| format!("创建文件失败：{}", self.destination.display()))?;
        Ok(())
    }
}

pub fn ensure_no_path_conflict(first: (&str, &Path), second: (&str, &Path)) -> Result<()> {
    let linked = first.1.try_exists()?
        && second.1.try_exists()?
        && same_file::is_same_file(first.1, second.1)?;
    if linked || path_key(first.1) == path_key(second.1) {
        bail!(
            "{}与{}路径冲突：{}；为避免破坏原始数据，已拒绝写入",
            first.0,
            second.0,
            first.1.display()
        );
    }
    Ok(())
}

/// 路径比较键：先做词法绝对化（折叠 `.`/`..`），再尽量用文件系统规范路径
/// 解析符号链接；Windows 下路径不区分大小写。
fn path_key(path: &Path) -> String {
    let absolute = absolute_lexical(path);
    let normalized = match absolute.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => match (absolute.parent(), absolute.file_name()) {
            (Some(parent), Some(name)) => parent
                .canonicalize()
                .map(|parent| parent.join(name))
                .unwrap_or_else(|_| absolute.clone()),
            _ => absolute.clone(),
        },
    };
    let text = normalized.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn absolute_lexical(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}
