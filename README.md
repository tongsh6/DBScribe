# DBScribe

**跨平台的数据库结构文档工具** —— 连上数据库，几十秒生成可交付的结构文档。

面向**个人开发者与小团队**：新接手项目摸清结构、项目交付附一份数据字典、给产品/测试解释表结构。
Windows / macOS / Linux 共用一套 Rust 代码，产物是单个可执行文件。

> 定位、目标用户与「不做什么」见 [docs/POSITIONING.md](docs/POSITIONING.md)。

---

## 状态

| 能力 | 状态 |
| --- | --- |
| MySQL / MariaDB | ✅ 可用 |
| PostgreSQL | ✅ 可用 |
| SQLite | ✅ 可用 |
| SQL Server | ✅ 可用 |
| Oracle | ✅ 可用（首次连接时自动下载客户端，约 60MB） |
| OceanBase | ✅ 可用（MySQL 兼容协议，`--driver oceanbase`） |
| 达梦 DM8 | ✅ 可用（Windows / Linux）：需先安装 DM ODBC 驱动，应用内会检测并给出安装指引；macOS 官方无驱动，不支持 |
| MongoDB | ✅ 可用：字段来自 collection validator（有声明时优先）或采样推断，文档会标注来源；已在 MongoDB 7.0 实例上完成集成验证 |
| Excel 导出（单 Sheet / 每表一个 Sheet） | ✅ 可用 |
| Markdown / HTML / JSON / XML 导出 | ✅ 可用 |
| 图形界面（Tauri 桌面应用） | ✅ 可用 |
| CHM 导出 | 不计划（将提供 HTML 站点替代） |

## 两种使用方式

- **桌面应用**：图形界面完成「连接 → 选库 → 浏览表 → 导出」全流程，适合日常使用
- **命令行**：可脚本化、可进 CI，适合批量与自动化

## 下载安装

从 [Releases](https://github.com/tongsh6/DBScribe/releases) 下载对应平台的产物，无需安装 Rust：

| 平台 | 桌面应用 | 命令行 |
| --- | --- | --- |
| Windows (x64) | `DBScribe_x.x.x_x64-setup.exe`（或 `.msi`） | `dbs-windows-x64.exe` |
| macOS (Apple Silicon) | `DBScribe_x.x.x_aarch64.dmg` | `dbs-macos-arm64` |
| Linux (x64) | `.deb` / `.rpm` / `.AppImage` | `dbs-linux-x64` |

命令行产物是单文件二进制，下载后直接运行即可：

```bash
chmod +x dbs-linux-x64 && ./dbs-linux-x64 --help
```

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

界面能力：

- **多连接管理**：保存多个数据源（主机/端口/用户），下拉随时切换；密码只存在内存，不落盘
- **按需勾选**：表列表支持搜索、全选 / 反选 / 全不选，只导出勾选的表
- **ER 图预览**：选表后可查看关系及推断理由，校对候选，保存离线 SVG 或供 CLI 复用的项目配置。
- **目录选择**：通过系统文件夹对话框选择导出目录
- **导出历史**：保留最近 30 条记录，点击任意一条可打开所在文件夹
- **结果反馈**：显示表数量、耗时，以及被自动改名的工作表清单

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
| `--driver` | 数据库类型：`mysql`（默认）/ `postgres` / `sqlite` / `sqlserver` / `oracle` / `oceanbase` / `dameng` / `mongodb` |
| `--host` / `--port` | 连接地址；端口缺省按类型取（mysql=3306，postgres=5432，sqlserver=1433，oracle=1521）；sqlite 不需要 |
| `--user` / `--password` | 账号；密码也可用环境变量 `DBS_PASSWORD` |
| `--database` | 要导出的库名 |
| `--out` | 输出 `.xlsx` 路径，目录会自动创建 |
| `--skip-partitioned` | 跳过数据库中的分区表 |
| `--sheet-mode` | `multi`（每表一个 Sheet，默认）或 `single`（全部表放一个 Sheet），仅 Excel 生效 |
| `--format` | 输出格式：`excel`（默认）/ `markdown` / `html` / `json` / `xml` / `word`；缺省按 `--out` 扩展名推断 |
| `--title` | 文档标题，缺省为「{数据库名}数据库设计文档」 |

### 导出 ER 图（离线 SVG）

MySQL 可从简单内连接视图中读取字段配对。推断指向可空唯一键时，SVG 会在线上标注“非空等值”。这表示完整键位均有值且按普通等号比较时，至多匹配一行。报告会给出指向该目标的依据。

在导出数据字典之外，可以基于同一份目录推断表关系并生成可离线查看的 SVG：

```bash
dbs er export \
  --driver sqlite \
  --database ./demo.db \
  --out ./demo-er.svg \
  --report ./demo-er.md
```

`--report` 可选，以 `.json` 结尾输出结构化结果，其他扩展名输出 Markdown。报告和 SVG 按本次选表交付；部分选表时，图外目标与全库诊断只计数。桌面校对面板仍可核对竞争目标及原始依据。桌面预览默认展开全部字段，按表头可收起或展开，保存 SVG 时保留当前状态。CLI 默认显示全部字段，可用 `--key-columns` 只显示键与关系字段。
SQLite 的 `--database` 直接填 `.db` 文件路径，`--host` / `--port` 不需要。

未声明外键的关系可依据完整键、字段用途、表列注释及库内独立的角色映射推断。SQLite 还可从结构明确的简单等值连接视图读取辅助证据。存在竞争或缺少依据时，候选只进入预览和报告，SVG 中不画候选线。

MongoDB 使用同一条 `dbs er export` 命令，指定 `--driver mongodb` 和库名即可；桌面端选中集合后点击“集合关系图预览”。导图只读目录，使用验证规则、索引和简单视图的 `$lookup` 定义。查询关联单独标注并显示视图出处，嵌入对象留在本集合内。普通数据字典仍沿用原有采样设置。

要复用勾选范围和人工修正，先在桌面 ER 预览中保存项目 JSON，再在同一条命令中加入 `--project <项目文件路径>`。

项目文件的 `scope` 必须与本次 `--driver` / `--host` / `--port` / `--database` 完全一致；不一致或缺少 `scope` 会直接报错。
`--out`、`--report` 与 SQLite 源库或项目文件指向同一路径（包括相对路径等价写法）时也会拒绝执行，避免覆盖原始数据。

MongoDB 的 `host` 可以直接写连接串；也可以像其他数据库一样分开传账号密码：

```bash
# 方式一：完整连接串（副本集 / TLS / authSource 都在里面）
dbs export --driver mongodb \
  --host 'mongodb://user@127.0.0.1:27017/?authSource=admin' \
  --password "$DBS_PASSWORD" \
  --database shop --out ./shop.md

# 方式二：直接写带密码的连接串（注意命令行历史）
dbs export --driver mongodb \
  --host 'mongodb://user:password@127.0.0.1:27017/' \
  --database shop --out ./shop.md
```

字段来自 collection validator（有声明时优先）或采样推断，文档会标注来源与采样覆盖率；
`--no-infer-fields` 可只导出权威结构，`--sample-size` 可调整采样条数（默认 200）。

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

围绕「把数据库文档这件事做到最好」：

1. **多数据库支持**：MySQL ✅ → PostgreSQL ✅ → SQLite ✅ → SQL Server ✅ → Oracle ✅ → 达梦 ✅ → MongoDB ✅
2. **文档质量**：索引信息、外键关系、ER 关系图
3. **交付体验**：文档模板、一次导出多个库
4. **CI 集成**：文档随代码自动更新

**明确不做** SQL 编辑器、数据库结构比对、实体类代码生成等——它们是别的产品要解决的问题，
详见 [docs/POSITIONING.md](docs/POSITIONING.md)。

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
