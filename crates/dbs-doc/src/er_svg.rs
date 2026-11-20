//! 离线 ER 图 SVG 渲染。
//!
//! 输入是已经裁决并按选表截取过的 [`ErDiagram`]，因此这里只负责稳定、可读地
//! 呈现表、字段、关系方向与来源，不再重新推断或改变关系。
//!
//! 布局使用确定性的网格：表按输入顺序排列，边从引用方边界指向被引用方边界。
//! 不依赖浏览器、外部渲染器、字体或时间戳，相同输入得到完全相同的 SVG。

use dbs_core::er::{Endpoint, ErDiagram, RelationKind, TableId};
use dbs_core::{ColumnInfo, TableInfo};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

const MARGIN: f64 = 48.0;
const NODE_HEADER_PAD_TOP: f64 = 11.0;
const NODE_HEADER_PAD_BOTTOM: f64 = 11.0;
const TITLE_LINE_H: f64 = 20.0;
const TABLE_COMMENT_LINE_H: f64 = 17.0;
const TABLE_COMMENT_GAP: f64 = 6.0;
const FIELD_LINE_H: f64 = 16.0;
const FIELD_TOP_PAD: f64 = 6.0;
const FIELD_BOTTOM_PAD: f64 = 7.0;
const FIELD_BLOCK_GAP: f64 = 6.0;
const FIELD_COMMENT_LINE_H: f64 = 14.0;
const MARKER_W: f64 = 42.0;
const NAME_MIN_W: f64 = 120.0;
const NAME_MAX_W: f64 = 240.0;
const TYPE_MIN_W: f64 = 90.0;
const TYPE_MAX_W: f64 = 200.0;
const NODE_PAD: f64 = 12.0;
const MIN_NODE_W: f64 = 260.0;
const MAX_NODE_W: f64 = 560.0;
const GAP_X: f64 = 320.0;
const GAP_Y: f64 = 88.0;
const MIN_CANVAS_W: f64 = 960.0;
const PANEL_MAX_CELLS: usize = 108;
// 标签能完整放进两列卡片的间隔，包括左右留白。
const EDGE_LABEL_MAX_CELLS: usize = 42;
const TITLE_CELL_W: f64 = 8.0;
const FIELD_CELL_W: f64 = 7.0;
const COMMENT_CELL_W: f64 = 6.4;
const FIELD_LABEL_GAP: f64 = 12.0;

/// 渲染一张可离线查看的 ER 图 SVG。
///
/// `show_all_columns` 为 `false` 时显示主键、唯一键和关系两端涉及的字段；
/// 为 `true` 时显示全部字段。孤立表、复合键、双角色与环线都会保留。
pub fn render_er_svg(diagram: &ErDiagram, title: &str, show_all_columns: bool) -> String {
    render_er_svg_with_collapsed(diagram, title, show_all_columns, &BTreeSet::new())
}

/// 与 [`render_er_svg`] 相同，但可以按完整 [`TableId`] 折叠表。
///
/// 折叠只隐藏该表的字段并压缩卡片高度，连线改落到表头；其他表的位置与画法不变。
pub fn render_er_svg_with_collapsed(
    diagram: &ErDiagram,
    title: &str,
    show_all_columns: bool,
    collapsed: &BTreeSet<TableId>,
) -> String {
    let selection = FieldSelection::collect(diagram);
    let mut nodes = build_nodes(diagram, &selection, show_all_columns, collapsed);
    let title_lines = wrap_display(title, 96);
    let (header_height, header_bottom) = header_geometry(title_lines.len());
    let (canvas_width, content_bottom) = place_nodes(&mut nodes, header_bottom);
    let panels = Panels::build(diagram);
    let panel_top = content_bottom + 42.0;
    let panel_bottom = panels.height_from(panel_top);
    let canvas_width = canvas_width.max(MIN_CANVAS_W);
    let canvas_height = (panel_bottom + 54.0).max(header_bottom + 120.0).max(320.0);

    let mut out = String::with_capacity(16 * 1024);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0}" height="{h:.0}" viewBox="0 0 {w:.0} {h:.0}" role="img">"#,
        w = canvas_width,
        h = canvas_height
    );
    let _ = writeln!(out, "<title>{}</title>", escape_xml(title));
    let _ = writeln!(
        out,
        "<desc>DBScribe 离线 ER 图：表、关系来源、方向与待判断候选</desc>"
    );
    write_defs(&mut out);
    let _ = writeln!(
        out,
        r##"<rect class="canvas" x="0" y="0" width="{:.0}" height="{:.0}"/>"##,
        canvas_width, canvas_height
    );

    write_header(
        &mut out,
        &title_lines,
        diagram,
        header_height,
        show_all_columns,
    );

    let (edge_svg, label_svg) = render_edges(diagram, &nodes, (canvas_width, canvas_height));
    out.push_str(&edge_svg);
    write_nodes(&mut out, &nodes);
    out.push_str(&label_svg);
    panels.write(&mut out, panel_top);
    let _ = writeln!(
        out,
        r#"<text class="footer" x="{:.1}" y="{:.1}">由 DBScribe 生成 · 离线 SVG · 无外部渲染依赖</text>"#,
        MARGIN,
        canvas_height - 22.0
    );
    out.push_str("</svg>\n");
    out
}

// ---------------------------------------------------------------------------
// 字段选择
// ---------------------------------------------------------------------------

/// 默认字段范围：主键、唯一键，以及任何关系/候选两端涉及的字段。
struct FieldSelection {
    relation: BTreeMap<TableId, BTreeSet<String>>,
    source: BTreeMap<TableId, BTreeSet<String>>,
    unique: BTreeMap<TableId, BTreeSet<String>>,
}

impl FieldSelection {
    fn collect(diagram: &ErDiagram) -> Self {
        let mut selection = Self {
            relation: BTreeMap::new(),
            source: BTreeMap::new(),
            unique: BTreeMap::new(),
        };
        for relation in &diagram.relations {
            selection.add_endpoint(&relation.source);
            selection.add_endpoint(&relation.target);
            if relation.kind == RelationKind::Declared {
                selection
                    .source
                    .entry(relation.source.table.clone())
                    .or_default()
                    .extend(relation.source.columns.iter().cloned());
            }
        }
        for key in &diagram.keys {
            selection
                .unique
                .entry(key.table.clone())
                .or_default()
                .extend(key.columns.iter().cloned());
        }
        selection
    }

    fn add_endpoint(&mut self, endpoint: &Endpoint) {
        self.relation
            .entry(endpoint.table.clone())
            .or_default()
            .extend(endpoint.columns.iter().cloned());
    }
}

/// 一个字段在卡片里的原始内容；键标记、名称、类型、说明始终分开保存。
struct FieldView {
    marker: String,
    name: String,
    data_type: String,
    comment: String,
}

fn selected_fields(
    table: &TableInfo,
    selection: &FieldSelection,
    show_all_columns: bool,
) -> Vec<FieldView> {
    let id = TableId::from(table);
    let relation = selection.relation.get(&id);
    let source = selection.source.get(&id);
    let unique = selection.unique.get(&id);
    let is_unique =
        |column: &ColumnInfo| unique.is_some_and(|columns| columns.contains(&column.name));
    let is_relation =
        |column: &ColumnInfo| relation.is_some_and(|columns| columns.contains(&column.name));
    let is_source =
        |column: &ColumnInfo| source.is_some_and(|columns| columns.contains(&column.name));
    table
        .columns
        .iter()
        .filter(|column| {
            show_all_columns || column.is_primary_key || is_unique(column) || is_relation(column)
        })
        .map(|column| {
            let mut markers = Vec::new();
            if column.is_primary_key {
                markers.push("PK");
            } else if is_unique(column) {
                markers.push("UQ");
            }
            if is_source(column) {
                markers.push("FK");
            }
            let data_type = if column.length.trim().is_empty() {
                column.data_type.trim().to_string()
            } else {
                format!("{}({})", column.data_type.trim(), column.length.trim())
            };
            FieldView {
                marker: markers.join("/"),
                name: column.name.clone(),
                data_type,
                comment: column.comment.trim().to_string(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 节点与布局
// ---------------------------------------------------------------------------

struct FieldLayout {
    marker: String,
    name_lines: Vec<String>,
    type_lines: Vec<String>,
    comment_lines: Vec<String>,
    /// 字段块顶部相对卡片顶部的高度。
    offset_y: f64,
    /// 类型栏相对卡片左侧的起点。
    type_offset: f64,
    height: f64,
}

struct NodeLayout {
    id: TableId,
    schema: String,
    name: String,
    title_lines: Vec<String>,
    table_comment_lines: Vec<String>,
    fields: Vec<FieldLayout>,
    anchors: BTreeMap<String, f64>,
    header_height: f64,
    collapsed: bool,
    width: f64,
    height: f64,
    x: f64,
    y: f64,
}

impl NodeLayout {
    fn anchor_y(&self, end: &Endpoint) -> f64 {
        if self.collapsed {
            return self.y + self.header_height / 2.0;
        }
        let offset = end
            .columns
            .first()
            .and_then(|column| self.anchors.get(column))
            .copied()
            .unwrap_or(self.header_height / 2.0);
        self.y + offset
    }

    fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            w: self.width,
            h: self.height,
        }
    }
}

fn build_nodes(
    diagram: &ErDiagram,
    selection: &FieldSelection,
    show_all_columns: bool,
    collapsed: &BTreeSet<TableId>,
) -> Vec<NodeLayout> {
    diagram
        .tables
        .iter()
        .map(|table| {
            let id = TableId::from(table);
            let title = table.name.clone();
            let table_comment = table.comment.trim().to_string();
            let is_collapsed = collapsed.contains(&id);
            let views = if is_collapsed {
                Vec::new()
            } else {
                selected_fields(table, selection, show_all_columns)
            };
            let max_name = views
                .iter()
                .map(|view| display_width(&view.name) as f64 * FIELD_CELL_W)
                .fold(0.0, f64::max);
            let max_type = views
                .iter()
                .map(|view| display_width(&view.data_type) as f64 * FIELD_CELL_W)
                .fold(0.0, f64::max);
            let title_natural = display_width(&title) as f64 * TITLE_CELL_W + NODE_PAD * 2.0;
            let comment_natural =
                display_width(&table_comment) as f64 * COMMENT_CELL_W + NODE_PAD * 2.0;
            let marker_w = MARKER_W;
            let required = NODE_PAD * 2.0
                + marker_w
                + FIELD_LABEL_GAP
                + max_name.max(NAME_MIN_W)
                + max_type.max(TYPE_MIN_W);
            let width = title_natural
                .max(comment_natural)
                .max(required)
                .clamp(MIN_NODE_W, MAX_NODE_W);
            let content_w = width - NODE_PAD * 2.0;
            let available = (content_w - marker_w - FIELD_LABEL_GAP).max(120.0);
            let natural_name = max_name.clamp(NAME_MIN_W, NAME_MAX_W);
            let natural_type = max_type.clamp(TYPE_MIN_W, TYPE_MAX_W);
            let extra = (available - natural_name - natural_type).max(0.0);
            let name_col_w = natural_name + extra * 0.6;
            let type_col_w = (available - name_col_w).max(48.0);
            let title_cells = (content_w / TITLE_CELL_W).floor().max(8.0) as usize;
            let comment_cells = (content_w / COMMENT_CELL_W).floor().max(8.0) as usize;
            let field_comment_cells =
                ((content_w - MARKER_W) / COMMENT_CELL_W).floor().max(8.0) as usize;
            let name_cells = (name_col_w / FIELD_CELL_W).floor().max(8.0) as usize;
            let type_cells = (type_col_w / FIELD_CELL_W).floor().max(6.0) as usize;

            let title_lines = wrap_display(&title, title_cells);
            let header_height = NODE_HEADER_PAD_TOP
                + title_lines.len() as f64 * TITLE_LINE_H
                + NODE_HEADER_PAD_BOTTOM;
            let mut cursor = header_height;
            let table_comment_lines = if table_comment.is_empty() {
                Vec::new()
            } else {
                let lines = wrap_display(&table_comment, comment_cells);
                cursor += TABLE_COMMENT_GAP + lines.len() as f64 * TABLE_COMMENT_LINE_H;
                lines
            };
            cursor += 4.0;

            let mut fields = Vec::new();
            let mut anchors = BTreeMap::new();
            let mut field_top = cursor;
            for view in views {
                if !fields.is_empty() {
                    field_top += FIELD_BLOCK_GAP;
                }
                let name_lines = wrap_display(&view.name, name_cells);
                let type_lines = if view.data_type.is_empty() {
                    Vec::new()
                } else {
                    wrap_display(&view.data_type, type_cells)
                };
                let comment_lines = if view.comment.is_empty() {
                    Vec::new()
                } else {
                    wrap_display(&view.comment, field_comment_cells)
                };
                let text_lines = name_lines.len().max(type_lines.len()).max(1);
                let height = FIELD_TOP_PAD
                    + text_lines as f64 * FIELD_LINE_H
                    + comment_lines.len() as f64 * FIELD_COMMENT_LINE_H
                    + FIELD_BOTTOM_PAD;
                let anchor_y = field_top + FIELD_TOP_PAD + FIELD_LINE_H / 2.0;
                anchors.insert(view.name.clone(), anchor_y);
                fields.push(FieldLayout {
                    marker: view.marker,
                    name_lines,
                    type_lines,
                    comment_lines,
                    offset_y: field_top,
                    type_offset: NODE_PAD + MARKER_W + name_col_w + FIELD_LABEL_GAP,
                    height,
                });
                field_top += height;
            }
            let body_height = if is_collapsed {
                cursor
            } else if fields.is_empty() {
                cursor + 24.0
            } else {
                field_top
            };
            NodeLayout {
                id,
                schema: table.schema.clone(),
                name: title,
                title_lines,
                table_comment_lines,
                fields,
                anchors,
                header_height,
                collapsed: is_collapsed,
                width,
                height: body_height + 8.0,
                x: 0.0,
                y: 0.0,
            }
        })
        .collect()
}

fn grid_columns(count: usize) -> usize {
    match count {
        0 | 1 => 1,
        2..=4 => 2,
        5..=9 => 3,
        _ => 4,
    }
}

/// 返回（画布宽度，节点区域底部）。
fn place_nodes(nodes: &mut [NodeLayout], start_y: f64) -> (f64, f64) {
    if nodes.is_empty() {
        return (MIN_CANVAS_W, start_y);
    }
    let columns = grid_columns(nodes.len());
    let rows = nodes.len().div_ceil(columns);
    let mut column_widths = vec![0.0_f64; columns];
    let mut row_heights = vec![0.0_f64; rows];
    for (index, node) in nodes.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        column_widths[column] = column_widths[column].max(node.width);
        row_heights[row] = row_heights[row].max(node.height);
    }
    let mut column_x = vec![MARGIN; columns];
    for column in 1..columns {
        column_x[column] = column_x[column - 1] + column_widths[column - 1] + GAP_X;
    }
    let mut row_y = vec![start_y; rows];
    for row in 1..rows {
        row_y[row] = row_y[row - 1] + row_heights[row - 1] + GAP_Y;
    }
    let mut right = MARGIN;
    let mut bottom = start_y;
    for (index, node) in nodes.iter_mut().enumerate() {
        let column = index % columns;
        let row = index / columns;
        node.x = column_x[column];
        node.y = row_y[row];
        right = right.max(node.x + node.width);
        bottom = bottom.max(node.y + node.height);
    }
    (right + MARGIN, bottom)
}

// ---------------------------------------------------------------------------
// 头部、图例与字段说明
// ---------------------------------------------------------------------------

fn header_geometry(title_lines: usize) -> (f64, f64) {
    let extra = title_lines.saturating_sub(1) as f64 * 26.0;
    let height = 170.0 + extra;
    (height, MARGIN + height)
}

fn write_header(
    out: &mut String,
    title_lines: &[String],
    diagram: &ErDiagram,
    header_height: f64,
    show_all_columns: bool,
) {
    let _ = writeln!(out, r#"<g class="header">"#);
    let first_baseline = MARGIN + 16.0;
    for (index, line) in title_lines.iter().enumerate() {
        let _ = writeln!(
            out,
            r#"<text class="doc-title" x="{:.1}" y="{:.1}">{}</text>"#,
            MARGIN,
            first_baseline + index as f64 * 26.0,
            escape_xml(line)
        );
    }
    let extra = (title_lines.len().saturating_sub(1)) as f64 * 26.0;
    let counts = kind_counts(diagram);
    let document = diagram.db_type == Some(dbs_core::DbType::MongoDb);
    let entity = if document { "集合" } else { "表" };
    let mut summary = format!(
        "{entity}：{} · 关系：{} · 候选：{} · 图外引用：{} · 提示：{}",
        diagram.tables.len(),
        diagram.relations.len(),
        diagram.candidates.len(),
        diagram.external_count,
        diagram.warnings.len()
    );
    if diagram.external_candidate_count > 0 {
        summary.push_str(&format!(
            " · 跨范围候选：{}",
            diagram.external_candidate_count
        ));
    }
    let summary_y = first_baseline + extra + 36.0;
    let _ = writeln!(
        out,
        r#"<text class="summary" x="{:.1}" y="{:.1}">{}</text>"#,
        MARGIN,
        summary_y,
        escape_xml(&summary)
    );
    write_legend(out, MARGIN, summary_y + 32.0, &counts, document);
    let fields_y = summary_y + 60.0;
    let fields = if show_all_columns {
        "字段：全部字段（含普通字段）"
    } else {
        "字段：主键 / 唯一键 / 关系字段"
    };
    let fields = format!("{fields} · 候选只计数，不画成连线");
    let _ = writeln!(
        out,
        r#"<text class="hint" x="{:.1}" y="{:.1}">{}</text>"#,
        MARGIN,
        fields_y,
        escape_xml(&fields)
    );
    let _ = writeln!(
        out,
        r#"<line class="rule" x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}"/>"#,
        MARGIN,
        MARGIN + header_height - 12.0,
        MIN_CANVAS_W - MARGIN,
        MARGIN + header_height - 12.0
    );
    let _ = writeln!(out, "</g>");
}

fn write_legend(
    out: &mut String,
    x: f64,
    y: f64,
    counts: &BTreeMap<EdgeKind, usize>,
    document: bool,
) {
    let mut entries = vec![
        (
            EdgeKind::Declared,
            "数据库声明",
            counts.get(&EdgeKind::Declared).copied().unwrap_or(0),
        ),
        (
            EdgeKind::Inferred,
            "系统推断",
            counts.get(&EdgeKind::Inferred).copied().unwrap_or(0),
        ),
        (
            EdgeKind::Confirmed,
            "人工确认",
            counts.get(&EdgeKind::Confirmed).copied().unwrap_or(0),
        ),
    ];
    if document {
        entries.retain(|(kind, _, _)| *kind != EdgeKind::Declared);
    }
    if document || counts.contains_key(&EdgeKind::Lookup) {
        entries.push((
            EdgeKind::Lookup,
            "视图查询关联",
            counts.get(&EdgeKind::Lookup).copied().unwrap_or(0),
        ));
    }
    let _ = writeln!(out, r#"<g class="legend">"#);
    for (index, (kind, label, count)) in entries.into_iter().enumerate() {
        let entry_x = x + index as f64 * 208.0;
        let _ = writeln!(
            out,
            r#"<line class="edge {}" x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}"/>"#,
            kind.css(),
            entry_x,
            y,
            entry_x + 30.0,
            y
        );
        let _ = writeln!(
            out,
            r#"<text class="legend-label" x="{:.1}" y="{:.1}">{}（{}）</text>"#,
            entry_x + 38.0,
            y + 4.0,
            escape_xml(label),
            count
        );
    }
    let _ = writeln!(out, "</g>");
}

// ---------------------------------------------------------------------------
// 边
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EdgeKind {
    Declared,
    Lookup,
    Inferred,
    Confirmed,
}

impl EdgeKind {
    fn css(self) -> &'static str {
        match self {
            EdgeKind::Declared => "declared",
            EdgeKind::Lookup => "lookup",
            EdgeKind::Inferred => "inferred",
            EdgeKind::Confirmed => "confirmed",
        }
    }

    fn label(self) -> &'static str {
        match self {
            EdgeKind::Declared => "数据库声明",
            EdgeKind::Lookup => "视图查询关联",
            EdgeKind::Inferred => "系统推断",
            EdgeKind::Confirmed => "人工确认",
        }
    }
}

struct EdgeVisual<'a> {
    source: &'a Endpoint,
    target: &'a Endpoint,
    kind: EdgeKind,
    non_null_equality: bool,
}

/// 同一对表的正反两个方向共用间距，避免环线两侧的标签叠在一起。
fn edge_pair(source: &TableId, target: &TableId) -> (TableId, TableId) {
    if source <= target {
        (source.clone(), target.clone())
    } else {
        (target.clone(), source.clone())
    }
}

fn kind_counts(diagram: &ErDiagram) -> BTreeMap<EdgeKind, usize> {
    let mut counts = BTreeMap::new();
    for relation in &diagram.relations {
        let kind = match relation.kind {
            RelationKind::Declared => EdgeKind::Declared,
            RelationKind::Lookup => EdgeKind::Lookup,
            RelationKind::Inferred => EdgeKind::Inferred,
            RelationKind::Confirmed => EdgeKind::Confirmed,
        };
        *counts.entry(kind).or_insert(0) += 1;
    }
    counts
}

/// 返回（边路径组，边标签组）。标签单独返回，以便画在节点上层。
fn render_edges(diagram: &ErDiagram, nodes: &[NodeLayout], canvas: (f64, f64)) -> (String, String) {
    let rects: BTreeMap<TableId, &NodeLayout> =
        nodes.iter().map(|node| (node.id.clone(), node)).collect();
    let mut edges: Vec<EdgeVisual<'_>> = diagram
        .relations
        .iter()
        .map(|relation| EdgeVisual {
            source: &relation.source,
            target: &relation.target,
            kind: match relation.kind {
                RelationKind::Declared => EdgeKind::Declared,
                RelationKind::Lookup => EdgeKind::Lookup,
                RelationKind::Inferred => EdgeKind::Inferred,
                RelationKind::Confirmed => EdgeKind::Confirmed,
            },
            non_null_equality: relation
                .condition
                .as_ref()
                .is_some_and(|c| c.non_null_equality),
        })
        .collect();
    edges.retain(|edge| {
        rects.contains_key(&edge.source.table) && rects.contains_key(&edge.target.table)
    });

    let mut totals: BTreeMap<(TableId, TableId), usize> = BTreeMap::new();
    for edge in &edges {
        *totals
            .entry(edge_pair(&edge.source.table, &edge.target.table))
            .or_insert(0) += 1;
    }
    let mut seen: BTreeMap<(TableId, TableId), usize> = BTreeMap::new();

    let mut svg = String::new();
    let mut labels = String::new();
    let _ = writeln!(svg, r#"<g class="edges">"#);
    let _ = writeln!(labels, r#"<g class="edge-labels">"#);
    for edge in &edges {
        let Some(source) = rects.get(&edge.source.table) else {
            continue;
        };
        let Some(target) = rects.get(&edge.target.table) else {
            continue;
        };
        let key = edge_pair(&edge.source.table, &edge.target.table);
        let total = totals.get(&key).copied().unwrap_or(1);
        let index = seen.entry(key).or_insert(0);
        let (path, curve) = edge_path(
            source.rect(),
            target.rect(),
            (source.anchor_y(edge.source), target.anchor_y(edge.target)),
            edge.source.table == edge.target.table,
            *index,
            total,
        );
        *index += 1;
        let _ = writeln!(
            svg,
            r#"<path class="edge {}" d="{}" marker-end="url(#arrow-{})"/>"#,
            edge.kind.css(),
            path,
            edge.kind.css()
        );
        let text = edge_label_text(edge);
        let lines = wrap_display(&text, EDGE_LABEL_MAX_CELLS);
        let size = edge_label_size(&lines);
        let point = choose_edge_label_point(curve, size, nodes, canvas);
        write_edge_label(&mut labels, point, &lines, size, edge.kind);
    }
    let _ = writeln!(svg, "</g>");
    let _ = writeln!(labels, "</g>");
    (svg, labels)
}

fn edge_label_text(edge: &EdgeVisual<'_>) -> String {
    let condition = if edge.non_null_equality {
        " · 非空等值"
    } else {
        ""
    };
    format!(
        "{} → {} · {}{}",
        edge.source.columns.join(" / "),
        edge.target.columns.join(" / "),
        edge.kind.label(),
        condition
    )
}

fn edge_label_size(lines: &[String]) -> (f64, f64) {
    let line_height = 14.0;
    let width = lines
        .iter()
        .map(|line| display_width(line) as f64 * 6.4)
        .fold(48.0, f64::max)
        + 12.0;
    let height = lines.len() as f64 * line_height + 6.0;
    (width, height)
}

/// 优先在边中部标注；边穿越卡片时使用最近的列间空白。耗时不随画布像素面积增长。
fn choose_edge_label_point(
    curve: Curve,
    size: (f64, f64),
    nodes: &[NodeLayout],
    canvas: (f64, f64),
) -> (f64, f64) {
    let half_w = size.0 / 2.0;
    let half_h = size.1 / 2.0;
    let clear = |point: (f64, f64)| {
        let label = Rect {
            x: point.0 - half_w - 3.0,
            y: point.1 - half_h - 3.0,
            w: size.0 + 6.0,
            h: size.1 + 6.0,
        };
        nodes.iter().all(|node| !rects_overlap(label, node.rect()))
    };
    let mid = curve.point(0.5);
    if clear(mid) {
        return mid;
    }
    for step in 1..=16 {
        let delta = step as f64 * (0.38 / 16.0);
        for t in [0.5 - delta, 0.5 + delta] {
            let point = curve.point(t);
            if clear(point) {
                return point;
            }
        }
    }
    let x = nodes
        .iter()
        .filter(|node| node.x > MARGIN)
        .map(|node| node.x - GAP_X / 2.0)
        .min_by(|a, b| (a - mid.0).abs().total_cmp(&(b - mid.0).abs()))
        .unwrap_or(canvas.0 - MARGIN - half_w);
    (x, mid.1)
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.y + b.h && b.y < a.y + a.h
}

fn write_edge_label(
    out: &mut String,
    point: (f64, f64),
    lines: &[String],
    size: (f64, f64),
    kind: EdgeKind,
) {
    let (width, height) = size;
    let line_height = 14.0;
    let top = point.1 - height / 2.0;
    let _ = writeln!(
        out,
        r#"<g class="edge-label"><rect class="edge-label-bg" x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="4"/>"#,
        point.0 - width / 2.0,
        top,
        width,
        height
    );
    let _ = write!(
        out,
        r#"<text class="edge-label-text {}" x="{:.1}" y="{:.1}" text-anchor="middle">"#,
        kind.css(),
        point.0,
        top + 12.0
    );
    for (index, line) in lines.iter().enumerate() {
        let dy = if index == 0 { 0.0 } else { line_height };
        let _ = write!(
            out,
            r#"<tspan x="{:.1}" dy="{:.1}">{}</tspan>"#,
            point.0,
            dy,
            escape_xml(line)
        );
    }
    let _ = writeln!(out, "</text></g>");
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Rect {
    fn cx(self) -> f64 {
        self.x + self.w / 2.0
    }

    fn right(self) -> f64 {
        self.x + self.w
    }
}

/// 一条边的三次贝塞尔曲线。
#[derive(Debug, Clone, Copy)]
struct Curve {
    start: (f64, f64),
    c1: (f64, f64),
    c2: (f64, f64),
    end: (f64, f64),
}

impl Curve {
    fn point(self, t: f64) -> (f64, f64) {
        bezier_point(self.start, self.c1, self.c2, self.end, t)
    }
}

/// 生成一条从引用方边界到被引用方边界的贝塞尔路径。
fn edge_path(
    source: Rect,
    target: Rect,
    anchors: (f64, f64),
    is_self: bool,
    index: usize,
    total: usize,
) -> (String, Curve) {
    let (sy, ty) = anchors;
    let offset = (index as f64 - total.saturating_sub(1) as f64 / 2.0) * 18.0;
    let (start, c1, c2, end) = if is_self || (source.x - target.x).abs() < 1.0 {
        let lane = source.right().max(target.right()) + 48.0 + index as f64 * 22.0;
        (
            (source.right(), sy),
            (lane, sy - 16.0),
            (lane, ty + 16.0),
            (target.right() + 3.0, ty),
        )
    } else {
        let rightward = source.cx() < target.cx();
        let (sx, tx, direction) = if rightward {
            (source.right(), target.x - 3.0, 1.0)
        } else {
            (source.x, target.right() + 3.0, -1.0)
        };
        let bend = ((tx - sx).abs() * 0.42).clamp(24.0, 140.0);
        (
            (sx, sy),
            (sx + direction * bend, sy + offset),
            (tx - direction * bend, ty + offset),
            (tx, ty),
        )
    };
    let curve = Curve { start, c1, c2, end };
    (path_d(start, c1, c2, end), curve)
}

fn path_d(start: (f64, f64), c1: (f64, f64), c2: (f64, f64), end: (f64, f64)) -> String {
    format!(
        "M {} {} C {} {} {} {} {} {}",
        number(start.0),
        number(start.1),
        number(c1.0),
        number(c1.1),
        number(c2.0),
        number(c2.1),
        number(end.0),
        number(end.1)
    )
}

fn bezier_point(
    start: (f64, f64),
    c1: (f64, f64),
    c2: (f64, f64),
    end: (f64, f64),
    t: f64,
) -> (f64, f64) {
    let u = 1.0 - t;
    (
        u * u * u * start.0 + 3.0 * u * u * t * c1.0 + 3.0 * u * t * t * c2.0 + t * t * t * end.0,
        u * u * u * start.1 + 3.0 * u * u * t * c1.1 + 3.0 * u * t * t * c2.1 + t * t * t * end.1,
    )
}

// ---------------------------------------------------------------------------
// 节点绘制
// ---------------------------------------------------------------------------

fn write_nodes(out: &mut String, nodes: &[NodeLayout]) {
    let _ = writeln!(out, r#"<g class="nodes">"#);
    for node in nodes {
        let rect = node.rect();
        let _ = writeln!(
            out,
            r#"<g class="node" data-schema="{}" data-table="{}">"#,
            escape_xml(&node.schema),
            escape_xml(&node.name)
        );
        let _ = writeln!(
            out,
            r#"<rect class="node-card" x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="9"/>"#,
            rect.x, rect.y, rect.w, rect.h
        );
        let _ = writeln!(
            out,
            r#"<rect class="node-header" x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="9"/>"#,
            rect.x + 1.0,
            rect.y + 1.0,
            rect.w - 2.0,
            node.header_height - 1.0
        );
        write_text_element(
            out,
            "node-title",
            rect.x + NODE_PAD,
            rect.y + NODE_HEADER_PAD_TOP + 15.0,
            &node.title_lines,
            TITLE_LINE_H,
        );
        let mut cursor = rect.y + node.header_height;
        if !node.table_comment_lines.is_empty() {
            cursor += TABLE_COMMENT_GAP;
            write_text_element(
                out,
                "table-comment",
                rect.x + NODE_PAD,
                cursor + 13.0,
                &node.table_comment_lines,
                TABLE_COMMENT_LINE_H,
            );
            cursor += node.table_comment_lines.len() as f64 * TABLE_COMMENT_LINE_H;
        }
        if node.collapsed {
            let _ = writeln!(out, "</g>");
            continue;
        }
        if node.fields.is_empty() {
            let _ = writeln!(
                out,
                r#"<text class="field empty" x="{:.1}" y="{:.1}">（当前范围无键或关联字段）</text>"#,
                rect.x + NODE_PAD,
                cursor + 17.0
            );
        } else {
            for (index, field) in node.fields.iter().enumerate() {
                let top = rect.y + field.offset_y;
                if index % 2 == 1 {
                    let _ = writeln!(
                        out,
                        r#"<rect class="field-row" x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}"/>"#,
                        rect.x + 1.0,
                        top,
                        rect.w - 2.0,
                        field.height - FIELD_BLOCK_GAP
                    );
                }
                let baseline = top + FIELD_TOP_PAD + 12.0;
                if !field.marker.is_empty() {
                    let _ = writeln!(
                        out,
                        r#"<text class="field-marker{}" x="{:.1}" y="{:.1}">{}</text>"#,
                        if field.marker.contains("PK") {
                            " key"
                        } else if field.marker.contains("FK") || field.marker.contains("UQ") {
                            " ref"
                        } else {
                            ""
                        },
                        rect.x + NODE_PAD,
                        baseline,
                        escape_xml(&field.marker)
                    );
                }
                write_text_element(
                    out,
                    "field-name",
                    rect.x + NODE_PAD + MARKER_W,
                    baseline,
                    &field.name_lines,
                    FIELD_LINE_H,
                );
                write_text_element(
                    out,
                    "field-type",
                    rect.x + field.type_offset,
                    baseline,
                    &field.type_lines,
                    FIELD_LINE_H,
                );
                if !field.comment_lines.is_empty() {
                    let text_lines = field.name_lines.len().max(field.type_lines.len()).max(1);
                    write_text_element(
                        out,
                        "field-comment",
                        rect.x + NODE_PAD + MARKER_W,
                        baseline + text_lines as f64 * FIELD_LINE_H + 2.0,
                        &field.comment_lines,
                        FIELD_COMMENT_LINE_H,
                    );
                }
            }
        }
        let _ = writeln!(out, "</g>");
    }
    let _ = writeln!(out, "</g>");
}

/// 一个 `<text>` 元素内按行写 `<tspan>`；完整文本始终真实出现在 SVG 中。
fn write_text_element(
    out: &mut String,
    class: &str,
    x: f64,
    first_baseline: f64,
    lines: &[String],
    line_height: f64,
) {
    if lines.is_empty() {
        return;
    }
    let _ = write!(
        out,
        r#"<text class="{}" x="{:.1}" y="{:.1}">"#,
        class, x, first_baseline
    );
    for (index, line) in lines.iter().enumerate() {
        let dy = if index == 0 { 0.0 } else { line_height };
        let _ = write!(
            out,
            r#"<tspan x="{:.1}" dy="{:.1}">{}</tspan>"#,
            x,
            dy,
            escape_xml(line)
        );
    }
    let _ = writeln!(out, "</text>");
}

// ---------------------------------------------------------------------------
// 候选与提示区
// ---------------------------------------------------------------------------

struct PanelLine {
    heading: bool,
    text: String,
}

struct Panels {
    lines: Vec<PanelLine>,
}

impl Panels {
    fn build(diagram: &ErDiagram) -> Self {
        let mut lines = Vec::new();
        if !diagram.warnings.is_empty() {
            lines.push(PanelLine {
                heading: true,
                text: "导出提示".into(),
            });
            for warning in &diagram.warnings {
                push_wrapped(&mut lines, &format!("• {warning}"), false);
            }
        }
        Self { lines }
    }

    fn height_from(&self, top: f64) -> f64 {
        let mut bottom = top;
        for line in &self.lines {
            bottom += if line.heading { 28.0 } else { 18.0 };
        }
        bottom
    }

    fn write(&self, out: &mut String, top: f64) {
        if self.lines.is_empty() {
            return;
        }
        let _ = writeln!(out, r#"<g class="panels">"#);
        let mut y = top;
        for line in &self.lines {
            y += if line.heading { 28.0 } else { 18.0 };
            let class = if line.heading {
                "panel-heading"
            } else {
                "panel-body"
            };
            let _ = writeln!(
                out,
                r#"<text class="{}" x="{:.1}" y="{:.1}">{}</text>"#,
                class,
                MARGIN,
                y,
                escape_xml(&line.text)
            );
        }
        let _ = writeln!(out, "</g>");
    }
}

fn push_wrapped(lines: &mut Vec<PanelLine>, text: &str, heading: bool) {
    for line in wrap_display(text, PANEL_MAX_CELLS) {
        lines.push(PanelLine {
            heading,
            text: line,
        });
    }
}

// ---------------------------------------------------------------------------
// 文本宽度、转义与 SVG 固定资源
// ---------------------------------------------------------------------------

fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(character: char) -> usize {
    if character.is_ascii() {
        1
    } else {
        2
    }
}

/// 按显示宽度换行；不做省略，完整文本始终写入可见的 `<tspan>`。
fn wrap_display(text: &str, max_cells: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let max_cells = max_cells.max(8);
    let mut result = Vec::new();
    for source_line in text.lines() {
        let mut line = String::new();
        let mut used = 0;
        for character in source_line.chars() {
            let width = char_width(character);
            if used + width > max_cells && !line.is_empty() {
                result.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(character);
            used += width;
        }
        result.push(line);
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c < ' ' && !matches!(c, '\t' | '\n' | '\r'))
                || matches!(c, '\u{fffe}' | '\u{ffff}') =>
            {
                out.push('\u{fffd}')
            }
            _ => out.push(character),
        }
    }
    out
}

fn number(value: f64) -> String {
    let value = if value.abs() < 0.05 { 0.0 } else { value };
    format!("{value:.1}")
}

fn write_defs(out: &mut String) {
    let _ = writeln!(out, r#"<defs>"#);
    let _ = writeln!(
        out,
        r#"<style>
.canvas {{ fill: #f7f9fc; }}
.doc-title {{ font: 700 22px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #12263f; }}
.summary {{ font: 13px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #334155; }}
.hint {{ font: 12px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #64748b; }}
.legend-label {{ font: 12px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #334155; }}
.rule {{ stroke: #d6deea; stroke-width: 1; }}
.footer {{ font: 11px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #94a3b8; }}
.node-card {{ fill: #ffffff; stroke: #cbd5e1; stroke-width: 1.2; }}
.node-header {{ fill: #eaf1f9; }}
.node-title {{ font: 600 13.5px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #17324d; }}
.table-comment {{ font: 11.5px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #64748b; }}
.field-row {{ fill: #f8fafc; }}
.field-marker {{ font: 700 10.5px "Cascadia Mono", Consolas, "Microsoft YaHei", monospace; fill: #64748b; }}
.field-marker.key {{ fill: #92400e; }}
.field-marker.ref {{ fill: #1d4ed8; }}
.field-name {{ font: 12px "Cascadia Mono", Consolas, "Microsoft YaHei", monospace; fill: #334155; }}
.field-type {{ font: 11.5px "Cascadia Mono", Consolas, "Microsoft YaHei", monospace; fill: #1d4ed8; }}
.field-comment {{ font: 11px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #64748b; }}
.field.empty {{ font: 12px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #94a3b8; font-style: italic; }}
.edge {{ fill: none; }}
.edge.declared {{ stroke: #1f4e79; stroke-width: 2; }}
.edge.inferred {{ stroke: #a15c00; stroke-width: 1.8; stroke-dasharray: 7 5; }}
.edge.confirmed {{ stroke: #166534; stroke-width: 2.8; }}
.edge.lookup {{ stroke: #623399; stroke-width: 2; stroke-dasharray: 9 3 2 3; }}
.edge-label-bg {{ fill: #ffffff; opacity: 0.93; stroke: #e2e8f0; stroke-width: 0.8; }}
.edge-label-text {{ font: 10.5px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #334155; }}
.edge-label-text.declared {{ fill: #1f4e79; }}
.edge-label-text.inferred {{ fill: #8a4f00; }}
.edge-label-text.confirmed {{ fill: #166534; }}
.edge-label-text.lookup {{ fill: #623399; }}
.panel-heading {{ font: 700 13px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #334155; }}
.panel-body {{ font: 11.5px "PingFang SC", "Microsoft YaHei", sans-serif; fill: #475569; }}
</style>"#
    );
    for (kind, color, hollow) in [
        ("declared", "#1f4e79", false),
        ("inferred", "#a15c00", false),
        ("confirmed", "#166534", false),
        ("lookup", "#623399", false),
    ] {
        let fill = if hollow { "none" } else { color };
        let stroke = if hollow { color } else { "none" };
        let _ = writeln!(
            out,
            r#"<marker id="arrow-{kind}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7.5" markerHeight="7.5" orient="auto"><path d="M 0 0 L 10 5 L 0 10 z" fill="{fill}" stroke="{stroke}" stroke-width="1.2"/></marker>"#
        );
    }
    let _ = writeln!(out, "</defs>");
}
