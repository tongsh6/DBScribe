# Changelog

本项目的所有重要变更都记录在此文件，格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

> 发布时由 `git-cliff` 依据 [cliff.toml](cliff.toml) 从 Conventional Commits 自动生成
> 对应 tag 的 Release Notes；本文件汇总历次版本。

## [Unreleased]

### 新功能

- **desktop**：导出进度实时反馈，显示「已完成 / 总表数」与进度条
- **desktop**：表列表支持按 表名 / 注释 / 列数 点击表头切换升降序
- **desktop**：搜索支持多关键词（空格分隔，全部命中）
- **desktop**：表结构抽屉中的列名与类型可点击复制
- **desktop**：记住上次使用的导出格式、Sheet 模式与导出目录
- **desktop**：快捷键 `Ctrl/Cmd+F` 聚焦搜索、`Esc` 关闭抽屉与弹窗

### 问题修复

- **desktop**：导出到已存在的文件时自动追加时间戳，不再静默覆盖旧文件
- **desktop**：导出历史按路径去重，避免同名记录堆积
- **core**：工作表名超长时改为首尾保留（`t_landlord…hitelist_file_member`），
  避免硬截断切断单词、以及截断后撞名被迫加 `_2` 后缀

### 构建与 CI

- 新增 Release Notes 机制：`cliff.toml` + git-cliff 在发布时自动生成分类说明
- 新增 `.github/release.yml`，为后续 PR 流程提供 GitHub 原生分类规则

## [0.2.0] - 2026-11-10

首个面向用户的版本：从 Windows 专用的 .NET 实现，重写为三平台可用的 Rust 应用。

### 新功能

- **core**：工作表名统一分配器，解决长表名 / 非法字符 / 大小写重复导致的导出失败
- **db**：MySQL / MariaDB 元数据读取，支持列出数据库
- **doc**：导出 Excel / Markdown / HTML / JSON / XML 五种格式
- **cli**：`dbs export` 与 `dbs databases` 命令，`--format` 可由扩展名推断
- **desktop**：Tauri 桌面应用，覆盖「连接 → 选库 → 浏览表 → 导出」完整流程
- **desktop**：多连接管理，可保存并切换多个数据源
- **desktop**：密码存入系统凭据管理器（Windows 凭据管理器 / macOS 钥匙串 / Linux Secret Service）
- **desktop**：按需勾选表、文件夹选择对话框、导出历史
- **desktop**：表结构预览抽屉、连接状态指示、加载反馈
- **desktop**：应用图标与界面 Logo

### 问题修复

- **ci**：关闭 pnpm 12 的供应链策略校验（`ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION`）
- **desktop**：修复 150% DPI 小视口下窗口超出屏幕、底部操作栏被挤出视口的问题

### 构建与 CI

- 三平台 CI（ubuntu / macOS / Windows）：核心库 fmt/clippy/test，桌面端前端构建 + cargo check
- 按路径分流触发，日常只做轻量校验；发布时才执行三平台完整构建
- Release 同时产出三平台 CLI 单文件二进制与桌面安装包

## [0.1.0] - 2026-11-10

项目初始化：Rust workspace 骨架（`dbs-core` / `dbs-db` / `dbs-doc` / `dbs-cli`）。

[Unreleased]: https://github.com/tongsh6/DBScribe/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/tongsh6/DBScribe/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tongsh6/DBScribe/releases/tag/v0.1.0
