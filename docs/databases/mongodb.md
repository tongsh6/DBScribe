# MongoDB 映射规范

> 本文档定义 MongoDB 在 DBScribe 中「如何映射成结构文档」，重点是**哪些内容是数据库声明的、哪些是工具采样猜的**。
> 映射规则一旦发布，用户导出的文档就会依赖它，因此先定规范、再实现。

## 为什么单独定规范

MongoDB 是文档型数据库，与现有七种关系型数据库有本质差异：

- **没有强制 schema**：字段可以随时出现、消失或改变类型，没有 DDL 可读
- **有声明层，但常被忽略**：collection validator（`$jsonSchema`）是 MongoDB 官方的 schema 声明机制，只是大多数库没有配置
- **有损映射**：没有列注释、外键、自增、分区这些概念
- **不可逆**：字段路径格式、类型展示、可空语义一旦被用户文档采用，改动就是破坏性的

因此本规范优先回答三个问题：**哪些是数据库说的、哪些是工具猜的、猜的部分如何标注**。

## 核心原则：声明优先，观察兜底，全程标注

| 原则 | 含义 | 为什么 |
| --- | --- | --- |
| 声明优先 | validator（`$jsonSchema`）存在时，字段结构以它为准 | 声明是静态事实，可复现、可进 git diff |
| 观察兜底 | 没有 validator 时，用采样推断补字段 | 否则 docs 里每张集合都是空字段表，交付价值为零 |
| 全程标注 | 每张表标注字段来源（声明 / 采样），采样标注样本数与时间 | 「文档 = 事实」是产品信任的根基，猜测必须可见 |

这条原则在别的领域同样成立：OpenAPI 文档优先读 schema、没有才抓流量；事件流优先读 Schema Registry、没有才采样本。**有声明用声明，没有才观察，观察必须带来源**。

## 信息分层

| 层 | 内容 | 来源 | 可复现 |
| --- | --- | --- | --- |
| **L1 声明层** | collection 列表、索引（含选项）、validator 字段结构 | `listCollections` / `listIndexes` | 是 |
| **L2 观察层** | 采样推断的字段、出现率、实测类型 | `$sample` 聚合 | 否（受采样时机与随机性影响） |

合并规则：

1. L1 有 validator 时，字段结构以声明为准；L2 只补声明未覆盖的字段，且逐个标注来源
2. L1 无 validator 时，字段结构整体退化为 L2，表级标注「采样推断」
3. validator 解析失败（脏数据、非常规写法）时，降级为 L2 并保留告警，**不得静默丢弃字段**

## 元数据映射总表

| MongoDB | DBScribe 模型 | 说明 |
| --- | --- | --- |
| database | 库 | 排除系统库 `admin` / `local` / `config` |
| collection | 表（`TableInfo`） | 单库导出时 `schema` 留空（与关系型单 schema 行为一致）；表注释为空 |
| validator `properties` | 列（`ColumnInfo`） | 声明来源，优先 |
| 文档字段 | 列（`ColumnInfo`） | 采样来源，兜底 |
| `_id` | 主键列 | `is_primary_key = true`，且固定排在首行 |
| `listIndexes` | 索引（`IndexInfo`） | 含复合索引、唯一索引、TTL、sparse、hidden |
| 无对应 | 外键 / 分区 / 自增 / 默认值 | 恒为空或 `false`，不臆造 |

- 未来支持一次导出多个 database 时，`schema` 填 database 名，复用 doc 层现有的「多 schema 自动加前缀」逻辑，不新增分支

## 字段来源与优先级

| 字段来源 | 触发条件 | 表级标注 | 列说明 |
| --- | --- | --- | --- |
| 声明 | validator 覆盖该字段 | `字段来源：collection validator` | validator 未要求必填时写 `可选（可能缺失）` |
| 采样 | 无 validator，或 validator 未覆盖该字段 | `字段来源：N 条采样推断（时间）` | 出现率 < 100% 时写 `出现率 87%` |
| 混合 | 同一集合两种来源都有 | 同时给出两条标注，并写明各自覆盖的字段数 | 按字段逐个标注 |

- 列说明在 MongoDB 上没有业务含义可用，正好用来承载「推断证据」，不新增列、不改 `ColumnInfo`
- 声明层字段排在前面，观察层补充字段排在后面；两层内部各自稳定排序

## 字段路径与嵌套

| 文档形态 | 展示为 |
| --- | --- |
| `{ "a": { "b": 1 } }` | `a.b` |
| `{ "tags": ["x", "y"] }` | `tags[]`，类型 `array<string>` |
| `{ "items": [{ "sku": "x" }] }` | `items[].sku` |
| 嵌套超过 3 层 | 只显示到第 3 层，更深层记为 `object`，并在表级标注「存在未展开层级」 |

- 数组只展开一层元素结构，不展开数组套数组
- validator 的 `properties` 与采样推断使用**同一套路径规则**，保证两种来源的列名可以直接合并

## 类型映射

| BSON / validator 类型 | 展示 |
| --- | --- |
| string | `string` |
| int32 / int | `int32` |
| int64 / long | `int64` |
| double | `double` |
| decimal128 / decimal | `decimal128` |
| bool | `bool` |
| date | `date` |
| objectId | `objectId` |
| binary / binData | `binary` |
| timestamp | `timestamp` |
| regex | `regex` |
| null | `null` |
| object | `object`（子字段另行展开） |
| array<T> | `array<T>` |
| 多类型（数组或混合样本） | 按出现次数降序并列，如 `string | int32` |
| 未声明类型 | `unknown`（不猜） |

- **长度列**：MongoDB 没有声明长度，留空，不展示「样本最大长度」
- **混合类型**：validator 的 `bsonType` 数组与采样的多类型观测，使用同一种并列展示

## 可空、必填与采样标注

`nullable` 只表示「值可以为 null」，与其他数据库语义保持一致：

| 场景 | `nullable` | 附加信息 |
| --- | --- | --- |
| 采样中出现过 BSON null | `true` | — |
| 采样中出现次数 < N | `false`（缺失 ≠ 空值） | 列说明写出现率 |
| validator `bsonType` 含 `null` | `true` | — |
| validator 未列入 `required` | 不变 | 列说明写 `可选（可能缺失）` |

**不要**把「字段缺失率」塞进 `nullable`：关系型的 `nullable` 是声明约束，MongoDB 的缺失是数据现实，混在一起会让导出文档的语义比其他库更模糊。

## 主键与索引

- `_id` → 主键；`is_auto_increment` 恒为 `false`（自增列整体不输出，见下）
- `listIndexes()` → 索引项：名称、列（按 key 顺序）、是否唯一、是否主键（`_id_`）
- 特殊索引（`$text` / `2dsphere` 等）索引列直接显示表达式 key，如 `$text`
- 索引选项写入列文本，保持 `IndexInfo` 模型不变：
  - TTL：`created_at (TTL 3600s)`
  - sparse：`email (sparse)`
  - hidden：`order_no (hidden)`
  - partial：`status (partial: {...})`

## 排序与可复现性

可复现是「文档进 git、随代码自动更新」的前提，MongoDB 需要显式处理：

| 对象 | 排序规则 | 理由 |
| --- | --- | --- |
| 库 | 字母序 | 与其他数据库一致 |
| collection | 字母序（导出前统一排序，序号随最终顺序重排） | 与其他数据库一致 |
| 字段 | `_id` 固定第一；声明层按 validator `properties` 顺序；观察层追加并按字母序 | `_id` 符合习惯，字母序保证两次采样结果可 diff |
| 索引 | 名称字母序 | 稳定 |

采样本身不确定，因此：

- 文档头与表级标注记录**采样时间、样本数、集合总文档数**，让读者知道这是哪一刻的画像
- 严格 diff 的确定性采样（`--sample-mode deterministic`）列为后续增强：v1 固定 `$sample`，靠字段字母序与来源标注保持可读的可比性

## 一致性要求

MongoDB 接入不得让现有格式产生「专属于 MongoDB 的分支」，以下通过既有开关自动满足：

| 要求 | 实现方式 |
| --- | --- |
| 不输出「自增」列 | `DbType::MongoDb.has_identity_column()` 返回 `false`，`column_headers` 自动少一列（与 Oracle 一致） |
| 不输出外键区块 / ER 图 | 外键恒空，Markdown 现有逻辑本就不输出 |
| 不输出「分区表」标记 | `is_partitioned` 恒 `false` |
| 单库导出表名不带库前缀 | `schema` 留空 |
| 默认端口 27017 | `ConnectionOptions::default_port` |
| 连接信息脱敏 | `describe()` 对连接串形式同样不输出密码 |

## 连接方式

- `host` 以 `mongodb://` 或 `mongodb+srv://` 开头时，**直接作为连接串使用**，可覆盖副本集、TLS、`authSource` 等场景
- 否则拼接：`mongodb://{user}:{password}@{host}:{port}/?authSource=admin`，密码做 URL 编码
- 默认端口：`27017`
- 不扩展 `ConnectionOptions` 模型（保持连接模型简单）

桌面端注意：host 识别为连接串时，`port` / `user` / `password` 输入框应置灰并提示「已按连接串解析」，避免用户填了连接串又填密码导致行为不可预期。

## 采样策略

| 项 | 取值 |
| --- | --- |
| 默认样本数 | 200 条 / collection |
| 采样方式 | 每个 collection 一次 `$sample`（或 `--sample-mode deterministic` 的排序截断） |
| 并发上限 | 4 个 collection 并行 |
| 空 collection | 仍输出该集合，字段为空列表（与关系型空表一致） |
| 可调参数 | CLI `--sample-size`（默认 200）、`--no-infer-fields`；桌面端使用默认值 |

采样是性能与准确度的折中：collection 很多时，每个都采样会变慢，因此需要并发上限并复用现有 export-progress 进度事件。

进度事件按当前「已完成表数 / 总表数」上报，采样与读索引共用同一计数，不引入新事件类型。

## 导出行为

- 列结构与关系型一致：序号 / 列名 / 数据类型 / 长度（空）/ 主键 / 允许空 / 默认值（空）/ 列说明（推断证据）
- 表级标注位置：Markdown 与 HTML 放在表标题下方；Excel 放在每个工作表表名下方的第一行；JSON / XML 作为结构化字段输出
- Excel：每个 collection 一个 Sheet（多 Sheet 模式），命名规则与其他数据库相同
- 多 database 场景与其他数据库一样，由用户在界面/CLI 中选择一个库后导出

## 明确不做

| 不做 | 原因 |
| --- | --- |
| 完整解析 `$jsonSchema`（pattern / enum / format / 条件校验） | 只取 `bsonType` / `required` / `properties` 已覆盖结构文档所需；完整语义属于校验，不是文档 |
| 展示样例文档内容 | 属于数据浏览，不是结构文档 |
| schema 校验 / 数据质量分析 | 超出「结构文档」范围 |
| 聚合管道、查询执行 | 与 POSITIONING 的「不做」一致 |
| 把推断结果伪装成声明 | 违反核心原则 |

## 声明源抽象与长期演进

本次只实现两个声明源，但抽象要留位置：

| 声明源 | 状态 | 可信度 | 说明 |
| --- | --- | --- | --- |
| collection validator | 本次实现 | 高 | 数据库内声明 |
| 采样观察 | 本次实现 | 中 | 数据画像 |
| Mongoose / Prisma 模型 | 后续可选 | 高 | 应用层声明，需读工程文件 |
| TypeScript 类型 | 后续可选 | 中高 | 可生成 JSON Schema |

- 驱动内部统一产出 `SchemaSource`（`Validator` / `Sampled`），文档层只认「来源」与「字段列表」，不为每个来源写分支
- 未来接 Mongoose / Prisma 时，只加一个「读取工程文件 → JSON Schema」的适配器，文档层与模型层零改动
- 应用层 schema 需要扫描用户工程文件，跨平台与权限成本高，因此列为「后续可选」，不进本次范围

长期看，MongoDB 支持的主轴不是「更好的采样」，而是**更多的声明源**：每接入一个声明源，文档可信度就上一个台阶，而采样始终只作为兜底。

## 被否决的方案

记录在这里，避免以后重复讨论：

| 方案 | 否决理由 |
| --- | --- |
| 纯采样推断（原方案） | 字段集合随机、不可复现，文档无法进 CI diff；交付文档里出现「猜测字段」却看不出是猜测，损害「文档 = 事实」的信任 |
| 只输出 collection + 索引，不做字段 | 每张表字段表为空，交付场景直接失效；且无法解释「为什么这个工具支持 MongoDB 却没有字段」 |
| 把 `$jsonSchema` 当「非通用约定」不做 | 判断错位：它正是 MongoDB 官方的声明层，是唯一可与关系型 DDL 对齐的信息源，成本只是约 150 行纯函数 |
| 新增「是否推断」列到 Excel | 破坏列结构与七种数据库的一致性，且每张表都多一列噪音；表级标注 + 列说明已足够 |
| 把缺失率写进 `nullable` | 语义与关系型冲突；缺失与空值是两件事 |

## 测试要求

- **纯函数单测**：字段路径展开、类型合并、可空判定、validator 解析、采样聚合、排序稳定性、标注文案
- **集成测试**：Docker 起 mongo，覆盖全 BSON 类型、嵌套、数组、复合索引、TTL 索引、validator 集合、无 validator 集合、认证连接
- **CI**：集成测试用 `#[ignore]` 标记，本地默认不跑；新增独立 job（仅 `ubuntu-latest` + `mongo:7` service）显式 `--ignored` 执行，不污染三平台矩阵

## 实现清单

| 文件 | 改动 |
| --- | --- |
| `crates/dbs-core/src/model.rs` | `DbType::MongoDb`、`from_str` 别名（`mongodb` / `mongo`）、`has_identity_column() = false` |
| `crates/dbs-db/src/mongodb.rs` | 驱动：库/集合列表、validator 解析、采样推断、索引、单集合列读取、声明/观察合并 |
| `crates/dbs-db/src/lib.rs` | 分发分支 |
| `crates/dbs-db/src/options.rs` | 默认端口 27017、连接串识别的脱敏描述 |
| `crates/dbs-db/Cargo.toml` | 依赖 `mongodb`（rustls 栈，与现有驱动一致；具体 feature 以 crate 当版本文档为准） |
| `crates/dbs-doc/src/common.rs` | 表级标注的统一生成入口（`table_note`），各格式复用 |
| `crates/dbs-doc/src/{markdown,html,excel,json,xml,word}.rs` | 调用表级标注；无标注时不输出空行 |
| `apps/desktop/src-tauri/src/commands.rs` | 命令分发 |
| `apps/desktop/src/App.tsx` | `DB_TYPES` 增加 MongoDB（端口 27017）；连接串识别提示 |
| `crates/dbs-cli/src/main.rs` | `--driver mongodb`、`--sample-size`、`--sample-mode`、`--no-infer-fields` |
| `crates/dbs-db/tests/mongodb.rs` | 集成测试（`#[ignore]`） |
| `.github/workflows/ci.yml` | 新增 `mongodb` job（ubuntu + `mongo:7` service） |

## 当前实现状态

| 能力 | 状态 |
| --- | --- |
| `DbType::MongoDb`、别名、默认端口、连接串与脱敏 | ✅ 已完成，含单测 |
| 集合 / 索引读取（TTL、sparse、hidden、partial、$text） | ✅ 已完成，含单测 |
| validator 解析（bsonType / required / 嵌套 / 数组） | ✅ 已完成，含单测 |
| 采样聚合（路径展开、类型合并、null 与出现率、3 层截断） | ✅ 已完成，含单测 |
| 声明优先 + 采样兜底 + 来源合并 | ✅ 已完成，含单测 |
| 表级来源标注（Markdown / HTML / Excel / Word / JSON / XML） | ✅ 已完成，含单测 |
| 桌面端数据库选项与真实 db_type 传递 | ✅ 已完成 |
| 采样参数（`--sample-size` / `--no-infer-fields`） | ✅ 已完成；`--sample-mode deterministic` 仍为后续 |
| 桌面端连接串置灰提示 | ✅ 已完成（host 识别为 `mongodb://` 时端口/账号/密码置灰） |
| Docker 集成测试与 CI job | ✅ 已接入：`#[ignore]` 集成测试 + 仅 ubuntu 的 `mongodb` job（mongo:7 service）；已在本地 mongo:7.0 实例验证通过 |

## 分步落地

1. **第一步（权威层）**：`DbType`、驱动骨架、库/集合列表、索引、`--no-infer-fields`；单测覆盖解析与排序。此步不产出推断字段，但可完整跑通连接与导出。
2. **第二步（声明层与观察层）**：validator 解析、采样推断、来源合并、表级标注；补集成测试与 CI job。
3. **第三步（体验）**：桌面端连接串提示、推断状态徽章、导出报告里展示「X 个集合来自声明、Y 个来自采样」。

第三步的「导出报告」是低成本高价值项：用户能一眼看出文档可信度分布，也导向他们为关键集合配置 validator。
