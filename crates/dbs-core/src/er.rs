//! ER 图推断、人工校对与展示范围使用的共享类型。
use crate::{DbType, TableInfo};
use serde::{Deserialize, Serialize};

mod conditional;
mod inference;
mod project;
pub use inference::{infer_relations, EvidenceKind};
pub use project::{
    build_delivery_diagram, build_diagram, deliver_diagram, make_decision, ErSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TableId {
    pub schema: String,
    pub name: String,
}

impl From<&TableInfo> for TableId {
    fn from(table: &TableInfo) -> Self {
        Self {
            schema: table.schema.clone(),
            name: table.name.clone(),
        }
    }
}

impl TableId {
    pub fn label(&self) -> String {
        if self.schema.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.schema, self.name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub table: TableId,
    pub columns: Vec<String>,
}

/// 只从目录读取的、可核对的简单等值比较；等号本身没有业务方向。
#[derive(Debug, Clone)]
pub struct ViewComparison {
    pub view: TableId,
    pub left: Endpoint,
    pub right: Endpoint,
}

#[derive(Debug, Clone)]
pub struct ErColumnType {
    pub table: TableId,
    pub column: String,
    pub signature: String,
}

#[derive(Debug, Default, Clone)]
pub struct ErCatalog {
    pub db_type: Option<DbType>,
    pub tables: Vec<TableInfo>,
    /// 仅包含驱动能够证明在全表有效、键位顺序正确的完整键。
    pub keys: Vec<Endpoint>,
    /// 目录能原样表达过滤条件的部分唯一键。不得加入全表可信键。
    pub guarded_keys: Vec<GuardedKey>,
    /// MySQL 普通唯一键的全部列有值且使用普通等值比较时，至多匹配一行。
    pub non_null_keys: Vec<Endpoint>,
    /// 视图列对含 NULL 安全等值，不能作为普通等值条件的依据。
    pub null_safe_views: Vec<ViewComparison>,
    pub types: Vec<ErColumnType>,
    pub views: Vec<ViewComparison>,
    /// 文档型目录中的查询关系和不满足自动条件的提示，不作为数据库外键。
    pub document_links: Analysis,
    /// 尚未能绑定到具体表、只能在全部选表时展示的全局/内部提示。
    pub warnings: Vec<String>,
    /// 生成时已绑定到具体表的提示；按选表范围过滤时只保留可见表的条目。
    pub scoped_warnings: Vec<ScopedWarning>,
}

/// 目录层提示的结构化归属。自由文本只在该表可见时展示。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScopedWarning {
    pub table: TableId,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Declared,
    Lookup,
    Inferred,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub source: Endpoint,
    pub target: Endpoint,
    pub kind: RelationKind,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<KeyCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub source: Endpoint,
    pub target: Endpoint,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<KeyCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarValue {
    Bool(bool),
    Integer(i64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RequiredEquality {
    pub column: String,
    pub value: ScalarValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedKey {
    pub key: Endpoint,
    pub required_target: Vec<RequiredEquality>,
}

/// 已配对的列见 Candidate 两端。目标索引的适用条件不是业务查询已经应用的条件。
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeyCondition {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub non_null_equality: bool,
    /// 交付选区图时保留指向依据的种类，无需携带可能点名图外对象的原文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_kind: Option<EvidenceKind>,
    pub required_target: Vec<RequiredEquality>,
    pub unbound_target_columns: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub relations: Vec<Relation>,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAction {
    Confirm,
    Exclude,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub source: Endpoint,
    pub target: Endpoint,
    pub action: DecisionAction,
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectScope {
    pub db_type: DbType,
    pub host: String,
    pub port: u16,
    pub database: String,
}

impl ProjectScope {
    pub fn label(&self) -> String {
        match self.db_type {
            DbType::Sqlite => format!(
                "sqlite://{}",
                self.database
                    .strip_prefix(r"\\?\")
                    .unwrap_or(&self.database)
            ),
            DbType::MongoDb => {
                // 也对从文件读到的旧作用域做脱敏，避免范围不匹配的错误消息回显凭证。
                let (scheme, address) = self
                    .host
                    .split_once("://")
                    .unwrap_or(("mongodb", self.host.as_str()));
                let seeds = address
                    .split(['/', '?', '#'])
                    .next()
                    .unwrap_or("")
                    .rsplit('@')
                    .next()
                    .unwrap_or("");
                format!("{scheme}://{seeds}/{}", self.database)
            }
            _ => format!(
                "{}://{}:{}/{}",
                self.db_type, self.host, self.port, self.database
            ),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ErProject {
    pub scope: Option<ProjectScope>,
    pub selected: Option<Vec<TableId>>,
    #[serde(default)]
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ErDiagram {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_type: Option<DbType>,
    pub tables: Vec<TableInfo>,
    #[serde(default)]
    pub keys: Vec<Endpoint>,
    pub relations: Vec<Relation>,
    pub candidates: Vec<Candidate>,
    pub external_count: usize,
    /// 来源可见、目标在图外的候选数量；交付投影只保留数量，不展开端点。
    #[serde(default)]
    pub external_candidate_count: usize,
    pub warnings: Vec<String>,
}
