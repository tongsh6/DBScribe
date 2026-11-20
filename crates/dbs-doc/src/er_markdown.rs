//! 离线 ER 关系报告（Markdown）。
//!
//! 输入必须是已经过交付投影的 [`ErDiagram`]：候选目标已被限制在当前选表
//! 范围内，生成理由也已收敛为只引用可见端点。本模块只做排版，不重新推断。

use dbs_core::er::{Endpoint, ErDiagram, ProjectScope, RelationKind, TableId};
use std::fmt::Write as _;

const NON_NULL_SCOPE: &str =
    "适用条件：两端全部键位非空并以普通等号逐列比较时，目标至多一行，也可能没有匹配行。";

/// 渲染与 CLI SVG/JSON、桌面导出共用的 Markdown 报告。
pub fn render_er_report(
    diagram: &ErDiagram,
    title: &str,
    scope: &ProjectScope,
    all_columns: bool,
) -> String {
    let mut report = String::new();
    let declared = diagram
        .relations
        .iter()
        .filter(|relation| relation.kind == RelationKind::Declared)
        .count();
    let inferred = diagram
        .relations
        .iter()
        .filter(|relation| relation.kind == RelationKind::Inferred)
        .count();
    let confirmed = diagram
        .relations
        .iter()
        .filter(|relation| relation.kind == RelationKind::Confirmed)
        .count();
    let lookup = diagram
        .relations
        .iter()
        .filter(|relation| relation.kind == RelationKind::Lookup)
        .count();
    let _ = writeln!(report, "# {title}\n");
    let _ = writeln!(report, "> ER 关系报告：完整来源、理由与候选\n");
    let _ = writeln!(report, "- 数据库：{}", scope.label());
    let _ = writeln!(
        report,
        "- 字段范围：{}",
        if all_columns {
            "全部字段"
        } else {
            "主键 / 唯一键 / 关系字段"
        }
    );
    let _ = writeln!(report, "- 实体：{}", diagram.tables.len());
    let _ = writeln!(
        report,
        "- 关系：{}（数据库声明 {} / 系统推断 {} / 人工确认 {} / 视图查询关联 {}）",
        diagram.relations.len(),
        declared,
        inferred,
        confirmed,
        lookup
    );
    let _ = writeln!(report, "- 候选：{}", diagram.candidates.len());
    let _ = writeln!(
        report,
        "- 跨范围候选：{}（只计数，不展开端点）",
        diagram.external_candidate_count
    );
    let _ = writeln!(report, "- 图外引用：{}\n", diagram.external_count);

    let _ = writeln!(report, "## 实体范围（{}）\n", diagram.tables.len());
    for table in &diagram.tables {
        let _ = writeln!(
            report,
            "- {}（{} 个字段）",
            TableId::from(table).label(),
            table.columns.len()
        );
    }

    let _ = writeln!(report, "\n## 关系（{}）\n", diagram.relations.len());
    if diagram.relations.is_empty() {
        let _ = writeln!(report, "无。\n");
    }
    for (index, relation) in diagram.relations.iter().enumerate() {
        let _ = writeln!(
            report,
            "### {}. [{}] {} → {}\n",
            index + 1,
            relation_kind_label(relation.kind),
            endpoint_text(&relation.source),
            endpoint_text(&relation.target)
        );
        let _ = writeln!(
            report,
            "- 来源：{}（{}）",
            relation_kind_label(relation.kind),
            relation_kind_slug(relation.kind)
        );
        if relation
            .condition
            .as_ref()
            .is_some_and(|c| c.non_null_equality)
        {
            let _ = writeln!(report, "- {NON_NULL_SCOPE}");
        }
        let _ = writeln!(report, "- 理由：{}\n", relation.reason);
    }

    let _ = writeln!(report, "## 候选（{}）\n", diagram.candidates.len());
    if diagram.candidates.is_empty() {
        let _ = writeln!(report, "无。\n");
    }
    for (index, candidate) in diagram.candidates.iter().enumerate() {
        let _ = writeln!(
            report,
            "### {}. {} → {}\n",
            index + 1,
            endpoint_text(&candidate.source),
            endpoint_text(&candidate.target)
        );
        let _ = writeln!(report, "- 状态：待判断候选（不会画成连线）");
        let _ = writeln!(report, "- 理由：{}\n", candidate.reason);
        if let Some(condition) = &candidate.condition {
            if condition.non_null_equality {
                let _ = writeln!(report, "- {NON_NULL_SCOPE}");
            }
            for term in &condition.required_target {
                let _ = writeln!(
                    report,
                    "- 目标索引条件：{} = {}（业务适用性待核对）",
                    term.column,
                    serde_json::to_string(&term.value).unwrap_or_default()
                );
            }
            if !condition.unbound_target_columns.is_empty() {
                let _ = writeln!(
                    report,
                    "- 尚未绑定的目标键位：{}",
                    condition.unbound_target_columns.join("、")
                );
            }
        }
    }

    let _ = writeln!(report, "## 导出提示（{}）\n", diagram.warnings.len());
    if diagram.warnings.is_empty() {
        let _ = writeln!(report, "无。\n");
    }
    for warning in &diagram.warnings {
        let _ = writeln!(report, "- {warning}");
    }
    let _ = writeln!(
        report,
        "\n---\n\n报告由 DBScribe 从本次读取的目录、项目文件与共享裁决生成；候选不参与自动连线。"
    );
    report
}

fn endpoint_text(endpoint: &Endpoint) -> String {
    format!(
        "{} ({})",
        endpoint.table.label(),
        endpoint.columns.join(", ")
    )
}

fn relation_kind_label(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Declared => "数据库声明",
        RelationKind::Lookup => "视图查询关联",
        RelationKind::Inferred => "系统推断",
        RelationKind::Confirmed => "人工确认",
    }
}

fn relation_kind_slug(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Declared => "declared",
        RelationKind::Lookup => "lookup",
        RelationKind::Inferred => "inferred",
        RelationKind::Confirmed => "confirmed",
    }
}
