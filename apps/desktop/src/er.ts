/** ER 面板共用的纯展示/合并逻辑，保持与后端 TableId / Endpoint 身份一致。 */
import type {
  Candidate,
  Decision,
  Endpoint,
  ErProject,
  RelationKind,
  TableIdentity,
} from "./types";

export function tableKey(table: TableIdentity): string {
  return `${table.schema}\u0000${table.name}`;
}

/** MySQL 的 schema 就是库名，界面上不必重复；多 schema 时保留限定名。 */
export function tableLabel(table: TableIdentity, database?: string): string {
  if (!table.schema || (database && table.schema === database)) return table.name;
  return `${table.schema}.${table.name}`;
}

export function endpointLabel(endpoint: Endpoint, database?: string): string {
  return `${tableLabel(endpoint.table, database)} (${endpoint.columns.join(", ")})`;
}

export function relationKindLabel(kind: RelationKind): string {
  switch (kind) {
    case "declared":
      return "数据库声明";
    case "confirmed":
      return "人工确认";
    case "lookup":
      return "视图查询关联";
    default:
      return "系统推断";
  }
}

export function relationKindClass(kind: RelationKind): string {
  return `kind-${kind}`;
}

export function decisionActionLabel(action: Decision["action"]): string {
  return action === "confirm" ? "采纳" : "排除";
}

export function sameEndpoint(left: Endpoint, right: Endpoint): boolean {
  return (
    sameTable(left.table, right.table) &&
    left.columns.length === right.columns.length &&
    left.columns.every((column, index) => column === right.columns[index])
  );
}

function sameTable(left: TableIdentity, right: TableIdentity): boolean {
  return left.schema === right.schema && left.name === right.name;
}

/**
 * 采纳时同一来源只保留一条人工确认（核心裁决也按来源互斥）；
 * 排除只替换同来源同目标，便于同时排除多个竞争候选。
 */
export function withDecision(project: ErProject, decision: Decision): ErProject {
  const decisions = project.decisions.filter((current) => {
    if (!sameEndpoint(current.source, decision.source)) return true;
    if (sameEndpoint(current.target, decision.target)) return false;
    return !(decision.action === "confirm" && current.action === "confirm");
  });
  return { ...project, decisions: [...decisions, decision] };
}

export function withoutDecision(project: ErProject, decision: Decision): ErProject {
  return {
    ...project,
    decisions: project.decisions.filter(
      (current) =>
        !(
          sameEndpoint(current.source, decision.source) &&
          sameEndpoint(current.target, decision.target)
        ),
    ),
  };
}

export function hasDecision(project: ErProject, candidate: Candidate): Decision | undefined {
  return project.decisions.find(
    (decision) =>
      sameEndpoint(decision.source, candidate.source) &&
      sameEndpoint(decision.target, candidate.target),
  );
}

export function safeFileName(value: string, fallback: string): string {
  const cleaned = value.replace(/[\\/:*?"<>|]/g, "_").trim();
  return cleaned || fallback;
}

/** SVG 卡片表头的实际坐标（viewBox 坐标系），用于对齐 HTML 点击层。 */
export interface ErHeaderBox {
  schema: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ErSvgLayout {
  width: number;
  height: number;
  headers: ErHeaderBox[];
}

function decodeXml(value: string): string {
  return value
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&amp;/g, "&");
}

const NODE_BLOCK = /<g class="node" data-schema="([^"]*)" data-table="([^"]*)">([\s\S]*?)<\/g>/g;
const NODE_HEADER = /<rect class="node-header" x="([-\d.]+)" y="([-\d.]+)" width="([-\d.]+)" height="([-\d.]+)"/;

/**
 * 解析后端自有 SVG 的卡片表头；只在预览变化时调用一次，
 * 鼠标事件直接命中缓存的布局，不再解析 SVG。
 */
export function parseErSvgLayout(svg: string): ErSvgLayout {
  const viewBox = svg.match(/viewBox="0 0 ([-\d.]+) ([-\d.]+)"/);
  const layout: ErSvgLayout = {
    width: viewBox ? Number(viewBox[1]) : 0,
    height: viewBox ? Number(viewBox[2]) : 0,
    headers: [],
  };
  if (!(layout.width > 0) || !(layout.height > 0)) return layout;
  for (const node of svg.matchAll(NODE_BLOCK)) {
    const header = node[3].match(NODE_HEADER);
    if (!header) continue;
    const [x, y, width, height] = header.slice(1).map(Number);
    if (![x, y, width, height].every(Number.isFinite) || width <= 0 || height <= 0) continue;
    layout.headers.push({
      schema: decodeXml(node[1]),
      name: decodeXml(node[2]),
      x,
      y,
      width,
      height,
    });
  }
  return layout;
}

/** 单表展开/收起；返回新集合，避免快速连点时共享可变状态。 */
export function toggledCollapsed(current: ReadonlySet<string>, key: string): Set<string> {
  const next = new Set(current);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  return next;
}

/** 批量展开或收起给定表，其余表的逐表状态保持不变。 */
export function setCollapsed(
  current: ReadonlySet<string>,
  keys: Iterable<string>,
  collapsed: boolean,
): Set<string> {
  const next = new Set(current);
  for (const key of keys) {
    if (collapsed) next.add(key);
    else next.delete(key);
  }
  return next;
}
