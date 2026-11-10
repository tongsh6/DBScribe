# DBScribe

**跨平台的数据库结构文档工具** —— 读取数据库表结构，一键生成设计文档。

Windows / macOS / Linux 共用一套 Rust 代码，产物是单个可执行文件。

---

## 状态

| 能力 | 状态 |
| --- | --- |
| MySQL / MariaDB 元数据读取 | ✅ 可用 |
| Excel 导出（单 Sheet / 每表一个 Sheet） | ✅ 可用 |
| Markdown / HTML / JSON / XML 导出 | ✅ 可用 |
| 图形界面（Tauri 桌面应用） | ✅ 可用 |
| PostgreSQL / SQL Server / SQLite / Oracle | 🚧 规划中 |
| CHM 导出 | 不计划（将提供 HTML 站点替代） |

## 两种使用方式

- **桌面应用**：图形界面完成「连接 → 选库 → 浏览表 → 导出」全流程，适合日常使用
- **命令行**：可脚本化、可进 CI，适合批量与自动化

## 快速开始

### 桌面应用

前置：Node.js 20+、pnpm、Rust 1.80+，以及各平台的 Tauri 依赖
（Windows 需 WebView2 与 MSVC 构建工具；macOS 需 Xcode 命令行工具；Linux 需 `libwebkit2gtk-4.1-dev` 等）。

```bash
cd apps/desktop
pnpm install
pnpm tauri dev      # 开发模式，热更新
pnpm tauri build    # 打包当前平台安装包
```

界面流程：填写连接参数 → 测试连接 → 选择数据库 → 读取表结构（可搜索）→ 选择格式 / 输出目录 → 导出，
结果区会显示表数量、耗时，以及被自动改名的工作表清单。

### 命令行

要求 Rust 1.80 及以上（`rustup` 安装即可，无需其他系统依赖）。

```bash
cargo build --release
./target/release/dbs --help
```

### 导出数据库文档

```bash
# 建议用环境变量传密码，避免写进命令历史
export DBS_PASSWORD='your-password'

dbs export \
  --driver mysql \
  --host 127.0.0.1 --port 3306 \
  --user root \
  --database your_database \
  --sheet-mode multi \
  --out ./your_database.xlsx
```

| 参数 | 说明 |
| --- | --- |
| `--driver` | 数据库类型，当前支持 `mysql` |
| `--host` / `--port` | 连接地址，端口缺省 3306 |
| `--user` / `--password` | 账号；密码也可用环境变量 `DBS_PASSWORD` |
| `--database` | 要导出的库名 |
| `--out` | 输出 `.xlsx` 路径，目录会自动创建 |
| `--sheet-mode` | `multi`（每表一个 Sheet，默认）或 `single`（全部表放一个 Sheet），仅 Excel 生效 |
| `--format` | 输出格式：`excel`（默认）/ `markdown` / `html` / `json` / `xml`；缺省按 `--out` 扩展名推断 |
| `--title` | 文档标题，缺省为「{数据库名}数据库设计文档」 |

先看看有哪些库：

```bash
dbs databases --host 127.0.0.1 --user root
```

导出的工作簿包含三部分：**修订日志**、**目录**（可点击跳转）、**表结构**（序号 / 列名 / 数据类型 / 长度 / 主键 / 自增 / 允许空 / 默认值 / 列说明）。

## 工作表命名与去重

Excel 对工作表名有硬性限制：最长 31 个字符、不能包含 `: \ / ? * [ ]`、去重时不区分大小写。
数据库里一旦出现长表名、同名不同大小写的表，直接拿表名建 Sheet 就会失败。

DBScribe 由 `SheetNameAllocator` 统一分配工作表名：

1. 清洗非法字符；
2. 按**字符数**截断到 31（不会切断多字节汉字）；
3. 大小写不敏感去重；
4. 冲突时追加 `_2` / `_3`；
5. 业务表名自动避让「目录」「修订日志」等固定工作表。

导出结束会打印改名映射，**任何表都不会丢**：

```text
导出成功：arcteryx_dev.xlsx
  表数量：390    工作表：392    模式：每表一个 Sheet
  有 80 个工作表因名称超长/非法/重复而被自动改名：
    t_exhibition_event_config  ->  t_exhibition_event_config_2
    t_landlord_presale_whitelist_file_member  ->  t_landlord_presale_whitelist__2
    t_store_themed_activities_questionnaire_option  ->  t_store_themed_activities_que_3
```

目录页的超链接显示原始表名，点击跳转到对应工作表；每个表页都带「返回目录」。

## 项目结构

```text
crates/
  dbs-core/   数据模型 + 工作表命名规则（纯逻辑，无外部依赖，含完整测试）
  dbs-db/     数据库元数据读取（information_schema）
  dbs-doc/    文档生成（Excel / Markdown / HTML / JSON / XML）
  dbs-cli/    命令行入口
apps/
  desktop/    桌面应用（Tauri 2 + React），后端命令复用 dbs-db / dbs-doc
legacy-dotnet/  早期 .NET 实现归档，仅作版式参照，不参与构建
```

分层原则：`dbs-core` 不依赖任何驱动或文件格式库，便于未来接入 GUI 与更多数据库。

## 路线图

1. **补全数据库**：PostgreSQL、SQL Server、SQLite、Oracle
2. **发布流程**：三平台安装包与 CLI 单文件二进制
3. **界面增强**：按需勾选表、导出历史、连接配置保存

## 开发

```bash
cargo fmt --all                                          # 格式化
cargo clippy --workspace --all-targets -- -D warnings    # 静态检查
cargo test --workspace                                   # 测试
```

CI 在 `ubuntu-latest` / `macos-latest` / `windows-latest` 三平台执行上述检查。

## 致谢与许可

- 部分设计参考 [dotnetchina/SmartSQL](https://gitee.com/dotnetchina/SmartSQL)（Apache-2.0），感谢原作者
- `legacy-dotnet/` 保留早期 .NET 实现，仅用于对照版式，不作为构建目标
- 本项目沿用 **Apache-2.0** 许可，详见 [LICENSE](LICENSE)
