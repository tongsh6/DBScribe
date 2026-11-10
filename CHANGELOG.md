# Changelog

本项目的所有重要变更都记录在此文件，格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

> 发布时由 `git-cliff` 依据 [cliff.toml](cliff.toml) 从 Conventional Commits 自动生成
> 对应 tag 的 Release Notes；本文件汇总历次版本。

## [0.3.0] - 2026-11-10

界面与交互的系统化重构：反馈体系、引导、错误可读性、设计规范、性能与可访问性。

### 新功能

- **desktop**：通知中心（铃铛 + 未读角标 + 可清理的消息栈），提示不再占用主布局
- **desktop**：右上角 Toast（4 秒自动消失），导出进度改为按钮内联显示
- **desktop**：首屏四步引导，按当前进度自动打勾与高亮
- **desktop**：错误解释层——把 MySQL / 文件系统的原始报错翻译成「发生了什么 + 该怎么办」
- **desktop**：表列表支持点击表头按 表名 / 注释 / 列数 切换升降序
- **desktop**：搜索支持多关键词（空格分隔，全部命中）
- **desktop**：表结构抽屉中列名与类型可点击复制
- **desktop**：记住导出设置（格式 / Sheet 模式 / 目录）与窗口尺寸位置
- **desktop**：启动自动连接（有保存密码时），选库后自动读取表结构
- **desktop**：键盘导航——上下键移动、空格勾选、回车预览，配合 `Ctrl+F` / `Esc`

### 问题修复

- **desktop**：导出到已存在文件时自动追加时间戳，不再静默覆盖旧文件
- **desktop**：导出历史按路径去重，避免同名记录堆积
- **desktop**：启动不再默认全屏，按屏幕尺寸自适应并居中
- **desktop**：表列表与抽屉启用 `content-visibility`，大库（390 表）滚动不再卡顿
- **core**：工作表名超长时改为首尾保留（`t_landlord…hitelist_file_member`），避免硬截断切断单词、以及截断后撞名被迫加后缀

### 样式与视觉

- **desktop**：建立设计 token 体系——圆角 8 种收敛为 3 档、字号 7 种收敛为 4 档、间距统一到 4px 栅格、37 处硬编码颜色归入语义变量
- **desktop**：自绘复选框、行 hover 过渡、输入框聚焦光晕、滚动条与选中文字美化

### 构建与 CI

- 接入 `git-cliff`：发布时依据 Conventional Commits 自动生成分类 Release Notes
- 新增 `CHANGELOG.md` 与 `.github/release.yml`（GitHub 原生分类规则）

### 文档

- 新增 [docs/POSITIONING.md](docs/POSITIONING.md)：明确项目定位、目标用户、核心价值与明确不做的范围

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

[0.3.0]: https://github.com/tongsh6/DBScribe/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tongsh6/DBScribe/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tongsh6/DBScribe/releases/tag/v0.1.0
