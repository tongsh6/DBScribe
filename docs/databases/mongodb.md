# MongoDB 映射规范

> 本文档定义 MongoDB 在 DBScribe 中「如何映射成结构文档」。
> 映射规则一旦发布，用户导出的文档就会依赖它，因此先定规范、再实现。

## 为什么单独定规范

MongoDB 是文档型数据库，与现有六种关系型数据库有本质差异：

- **没有固定 schema**：字段类型只能通过采样推断，不是读出来的
- **有损映射**：没有列注释、外键、自增、分区这些概念
- **不可逆**：字段路径格式、类型展示、可空语义一旦被用户文档采用，改动就是破坏性的

因此本规范优先回答「哪些是确定的、哪些是推断的、哪些不做」。

## 元数据映射总表

| MongoDB | DBScribe 模型 | 说明 |
| --- | --- | --- |
| database | 库 | 排除系统库 `admin` / `local` / `config` |
| collection | 表（`TableInfo`） | `schema` 填所属 database 名；表注释为空 |
| 文档字段 | 列（`ColumnInfo`） | 采样推断，不是数据库声明 |
| `_id` 字段 | 主键列 | `is_primary_key = true` |
| `listIndexes` | 索引（`IndexInfo`） | 含复合索引、唯一索引 |
| 无对应 | 外键 / 分区 / 自增 / 默认值 / 列说明 | 恒为空或 `false` |

## 字段路径与嵌套

| 文档形态 | 展示为 |
| --- | --- |
| `{ "a": { "b": 1 } }` | `a.b` |
| `{ "tags": ["x", "y"] }` | `tags[]`，类型 `array<string>` |
| `{ "items": [{ "sku": "x" }] }` | `items[].sku` |
| 嵌套超过 3 层 | 只显示到第 3 层，更深层记为 `object` |

- 字段顺序：按样本中**首次出现顺序**；同一 collection 的多次读取顺序稳定
- 数组只展开一层元素结构，不展开数组套数组

## 类型映射

| BSON 类型 | 展示 |
| --- | --- |
| string | `string` |
| int32 / int64 | `int32` / `int64` |
| double | `double` |
| decimal128 | `decimal128` |
| bool | `bool` |
| date | `date` |
| objectId | `objectId` |
| binary | `binary` |
| timestamp | `timestamp` |
| regex | `regex` |
| null | `null` |
| object | `object`（子字段另行展开） |
| array<T> | `array<T>` |

- **混合类型**：同一字段在样本中出现多种类型时，按出现次数降序并列，如 `string | int32`
- **长度列**：MongoDB 没有声明长度，留空，不展示「样本最大长度」

## 可空与推断声明

- 采样 N 条，字段出现次数 = N → `nullable = false`
- 出现次数 < N → `nullable = true`
- 文档中注明「基于 N 条采样推断」，不宣称是数据库约束

## 主键与索引

- `_id` → 主键；`is_auto_increment` 恒为 `false`
- `listIndexes()` → 索引项：名称、列（按 key 顺序）、是否唯一、是否主键（`_id_`）
- 特殊索引（`$text` / `2dsphere` 等）索引列直接显示表达式的 key，如 `$text`

## 无对应概念

以下字段对 MongoDB 恒为空或 `false`，实现不得臆造：

- `comment`、`default_value`、`is_auto_increment`
- `foreign_keys`、`is_partitioned`
- 导出层对 MongoDB **不输出 Mermaid ER 图**（没有外键关系）

## 连接方式

- `host` 字段以 `mongodb://` 或 `mongodb+srv://` 开头时，**直接作为连接串使用**，可覆盖副本集、TLS、`authSource` 等场景
- 否则拼接：`mongodb://{user}:{password}@{host}:{port}/?authSource=admin`，密码做 URL 编码
- 默认端口：`27017`
- 不扩展 `ConnectionParams` 模型（保持连接模型简单）

## 采样策略

| 项 | 取值 |
| --- | --- |
| 默认样本数 | 200 条 / collection |
| 采样方式 | 每个 collection 一次 `$sample` |
| 并发上限 | 4 个 collection 并行 |
| 空 collection | 仍输出该集合，字段为空列表（与关系型空表一致） |
| 可调参数 | CLI `--sample-size`；桌面端使用默认值 |

采样是性能与准确度的折中：collection 很多时，每个都采样会变慢，因此需要并发上限并复用现有 `export-progress` 进度事件。

## 导出行为

- 列结构与关系型一致：序号 / 列名 / 数据类型 / 长度（空）/ 主键 / 自增（空）/ 允许空 / 默认值（空）/ 列说明（空）
- Excel：每个 collection 一个 Sheet（多 Sheet 模式），命名规则与其他数据库相同
- Markdown / HTML / JSON / XML / Word：与现有格式一致
- 多 database 场景与其他数据库一样，由用户在界面/CLI 中选择一个库后导出

## 明确不做

| 不做 | 原因 |
| --- | --- |
| 解析 `$jsonSchema` 作为注释 | 非通用约定，先不引入 |
| 展示样例文档内容 | 属于数据浏览，不是结构文档 |
| schema 校验 / 数据质量分析 | 超出「结构文档」范围 |
| 聚合管道、查询执行 | 与 POSITIONING 的「不做」一致 |

## 测试要求

- **纯函数单测**：字段路径展开、类型合并、可空判定、采样聚合
- **集成测试**：Docker 起 mongo，覆盖全 BSON 类型、嵌套、数组、复合索引、认证连接
- **CI**：集成测试接入 GitHub Actions service，长期保证回归

## 实现清单

| 文件 | 改动 |
| --- | --- |
| `crates/dbs-core/src/model.rs` | `DbType::MongoDb` 与 `from_str` 别名 |
| `crates/dbs-db/src/mongodb.rs` | 驱动：库/集合列表、采样推断、索引、单集合列读取 |
| `crates/dbs-db/src/lib.rs` | 分发分支 |
| `crates/dbs-db/Cargo.toml` | 依赖 `mongodb`（tokio + rustls） |
| `apps/desktop/src-tauri/src/commands.rs` | 命令分发 |
| `apps/desktop/src/App.tsx` | `DB_TYPES` 增加 MongoDB（端口 27017） |
| `crates/dbs-cli/src/main.rs` | `--driver mongodb`、`--sample-size` |
| `crates/dbs-db/tests/mongodb.rs` | 集成测试 |
| `.github/workflows/ci.yml` | mongo service 与集成测试 |
