# DBScribe

**跨平台的数据库结构文档工具** —— 读取数据库表结构，一键生成设计文档。

Windows / macOS / Linux 共用一套 Rust 代码，产物是单个可执行文件。

> 本项目是 [dotnetchina/SmartSQL](https://gitee.com/dotnetchina/SmartSQL) 的跨平台重写。
> 上游基于 .NET Framework + WPF，只能运行在 Windows；DBScribe 用 Rust 重新实现其核心能力，
> 并修复了上游「多 Sheet 导出必然失败」的缺陷（详见下文）。

---

## 状态

| 能力 | 状态 |
| --- | --- |
| MySQL / MariaDB 元数据读取 | ✅ 可用 |
| Excel 导出（单 Sheet / 每表一个 Sheet） | ✅ 可用 |
| PostgreSQL / SQL Server / SQLite / Oracle | 🚧 规划中 |
| Markdown / HTML / JSON / XML 导出 | 🚧 规划中 |
| 图形界面 | 🚧 规划中 |
| CHM 导出 | ⚠️ 上游依赖 Windows 专有的 `hhc.exe`，跨平台版本将以 HTML 站点替代 |

## 快速开始

### 构建

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
| `--sheet-mode` | `multi`（每表一个 Sheet，默认）或 `single`（全部表放一个 Sheet） |
| `--title` | 文档标题，缺省为「{数据库名}数据库设计文档」 |

导出的工作簿包含三部分：**修订日志**、**目录**（可点击跳转）、**表结构**（序号 / 列名 / 数据类型 / 长度 / 主键 / 自增 / 允许空 / 默认值 / 列说明）。

---

## 修复了上游的什么缺陷

### 多 Sheet 导出「没有任何结果」

**现象**：SmartSQL 1.0.4.2 选择「多 Sheet（每个表一个 Sheet）」导出 Excel 后，目标目录里没有生成任何文件，也没有明显的错误提示。

**根因**（反编译 `SmartSQL.DocUtils.dll` 确认）：

多 Sheet 分支把数据库表名**原样**当作 Excel 工作表名：

```csharp
foreach (TableDto item in tables)
    CreateTableSheet(val, item.TableName, dto, new List<TableDto> { item });
```

而 Excel 对工作表名的限制是：**最长 31 个字符、不能包含 `: \ / ? * [ ]`、去重时不区分大小写**。
EPPlus 4.0.5 会先截断、替换非法字符，再去重；一旦两张表截断后同名（或仅大小写不同），
`Worksheets.Add()` 就抛 `InvalidOperationException: A worksheet with this name already exists`。

此时 `ExportExcelByEpplus` **已经删除了同名旧文件**，而 `Save()` 还没执行 ——
结果是：旧文件被删，新文件没生成。错误提示又是 HandyControl 的 Growl（`WaitTime = 1` 秒），
一闪而过，看起来就像「点了没反应」。

**典型触发条件**（一个 390 张表的真实库一次命中 6 组）：

- `T_EXHIBITION_EVENT_CONFIG` 与 `t_exhibition_event_config` —— 仅大小写不同
- `t_landlord_presale_whitelist_file` 与 `t_landlord_presale_whitelist_file_member` —— 截断到 31 字符后同名
- `t_store_themed_activities_questionnaire` / `_answer` / `_option` / `_question` —— 四张表截断后同名

**DBScribe 的做法**：由 `SheetNameAllocator` 统一分配工作表名 ——
清洗非法字符 → 按**字符数**截断（不会切断多字节汉字）→ 大小写不敏感去重 →
冲突时追加 `_2` / `_3`，并让业务表名自动避让「目录」「修订日志」等固定工作表。
导出结束会打印改名映射，**任何表都不会丢**：

```text
导出成功：arcteryx_dev.xlsx
  表数量：390    工作表：392    模式：每表一个 Sheet
  有 80 个工作表因名称超长/非法/重复而被自动改名（原版会直接导出失败）：
    t_exhibition_event_config  ->  t_exhibition_event_config_2
    t_landlord_presale_whitelist_file_member  ->  t_landlord_presale_whitelist__2
    t_store_themed_activities_questionnaire_option  ->  t_store_themed_activities_que_3
    ...
```

---

## 项目结构

```text
crates/
  dbs-core/   数据模型 + 工作表命名规则（纯逻辑，无外部依赖，含完整测试）
  dbs-db/     数据库元数据读取（information_schema）
  dbs-doc/    文档生成（Excel / 后续 Markdown、HTML、JSON…）
  dbs-cli/    命令行入口
legacy-dotnet/  上游 .NET 实现归档，仅作功能与版式参照，不参与构建
```

分层原则：`dbs-core` 不依赖任何驱动或文件格式库，便于未来接入 GUI 与更多数据库。

## 路线图

1. **补全数据库**：PostgreSQL、SQL Server、SQLite、Oracle
2. **补全导出格式**：Markdown、HTML、JSON、XML（对齐上游能力）
3. **图形界面**：优先 Web 前端 + 本地后端，也可打包桌面壳，三平台共用
4. **发布流程**：GitHub Actions 三平台构建，产出 `dbs` 单文件二进制

## 开发

```bash
cargo fmt --all                                   # 格式化
cargo clippy --workspace --all-targets -- -D warnings   # 静态检查
cargo test --workspace                            # 测试
```

CI 在 `ubuntu-latest` / `macos-latest` / `windows-latest` 三平台执行上述检查。

## 致谢与许可

- 上游项目：[dotnetchina/SmartSQL](https://gitee.com/dotnetchina/SmartSQL)（Apache-2.0），感谢原作者的工作
- `legacy-dotnet/` 保留上游 .NET 实现，用于对照功能与版式，不作为构建目标
- 本项目沿用 **Apache-2.0** 许可，详见 [LICENSE](LICENSE)
