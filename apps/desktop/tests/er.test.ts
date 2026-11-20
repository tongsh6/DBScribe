import test from "node:test";
import assert from "node:assert/strict";
import type { Decision, Endpoint, ErProject } from "../src/types.ts";
import {
  parseErSvgLayout,
  relationKindLabel,
  sameEndpoint,
  safeFileName,
  setCollapsed,
  tableKey,
  tableLabel,
  toggledCollapsed,
  withDecision,
  withoutDecision,
} from "../src/er.ts";

test("集合视图查询关联有独立来源标签", () => {
  assert.equal(relationKindLabel("lookup"), "视图查询关联");
});

function endpoint(schema: string, name: string, columns: string[] = ["id"]): Endpoint {
  return { table: { schema, name }, columns };
}

function decision(
  source: Endpoint,
  target: Endpoint,
  action: Decision["action"] = "confirm",
): Decision {
  return { source, target, action, basis: "v1-test" };
}

const source = endpoint("public", "orders", ["user_id"]);
const baseProject: ErProject = {
  scope: null,
  selected: null,
  decisions: [],
};

test("同名表用 schema 区分，MySQL 库名不重复显示", () => {
  assert.notEqual(
    tableKey({ schema: "public", name: "users" }),
    tableKey({ schema: "sales", name: "users" }),
  );
  assert.equal(tableLabel({ schema: "app", name: "users" }, "app"), "users");
  assert.equal(tableLabel({ schema: "public", name: "users" }, "app"), "public.users");
});

test("端点比较要求字段顺序一致", () => {
  assert.equal(
    sameEndpoint(endpoint("public", "users", ["a", "b"]), endpoint("public", "users", ["a", "b"])),
    true,
  );
  assert.equal(
    sameEndpoint(endpoint("public", "users", ["a", "b"]), endpoint("public", "users", ["b", "a"])),
    false,
  );
});

test("采纳替换同一来源的旧确认，但保留其他目标的排除", () => {
  const oldConfirm = decision(source, endpoint("public", "users", ["id"]));
  const excluded = decision(source, endpoint("sales", "users", ["id"]), "exclude");
  const nextConfirm = decision(source, endpoint("auth", "users", ["id"]));
  const project: ErProject = {
    ...baseProject,
    decisions: [oldConfirm, excluded],
  };

  const next = withDecision(project, nextConfirm);
  assert.deepEqual(
    next.decisions.map((item) => `${item.target.table.schema}.${item.action}`),
    ["sales.exclude", "auth.confirm"],
  );
});

test("撤销只移除同来源同目标，不影响其他修正", () => {
  const confirmed = decision(source, endpoint("public", "users", ["id"]));
  const excluded = decision(source, endpoint("sales", "users", ["id"]), "exclude");
  const project: ErProject = { ...baseProject, decisions: [confirmed, excluded] };

  const next = withoutDecision(project, confirmed);
  assert.equal(next.decisions.length, 1);
  assert.equal(next.decisions[0].target.table.schema, "sales");
});

test("同一张表的不同来源字段组互不覆盖", () => {
  const byUser = endpoint("public", "orders", ["user_id"]);
  const byTenant = endpoint("public", "orders", ["tenant_id"]);
  const userDecision = decision(byUser, endpoint("public", "users", ["id"]));
  const tenantDecision = decision(byTenant, endpoint("public", "tenants", ["id"]));
  const project: ErProject = { ...baseProject, decisions: [userDecision] };

  const next = withDecision(project, tenantDecision);
  assert.equal(next.decisions.length, 2);
});

test("文件名清理非法字符并保留兜底", () => {
  assert.equal(safeFileName('a/b:c*?"<>|', "er"), "a_b_c______");
  assert.equal(safeFileName("   ", "er"), "er");
});

const svgFixture = `<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600" viewBox="0 0 800 600">
<g class="node" data-schema="public" data-table="orders">
<rect class="node-header" x="10" y="20" width="180" height="30" rx="9"/>
</g>
<g class="node" data-schema="sales" data-table="a&amp;b">
<rect class="node-header" x="300" y="120.5" width="200" height="36" rx="9"/>
</g>
</svg>`;

test("只解析一次 SVG 表头坐标并保留完整 schema 身份", () => {
  const layout = parseErSvgLayout(svgFixture);
  assert.equal(layout.width, 800);
  assert.equal(layout.height, 600);
  assert.deepEqual(layout.headers, [
    { schema: "public", name: "orders", x: 10, y: 20, width: 180, height: 30 },
    { schema: "sales", name: "a&b", x: 300, y: 120.5, width: 200, height: 36 },
  ]);
  assert.equal(parseErSvgLayout("<svg></svg>").headers.length, 0);
});

test("逐表收起按完整 TableId 区分同名表，批量只改已选范围", () => {
  const publicKey = tableKey({ schema: "public", name: "users" });
  const salesKey = tableKey({ schema: "sales", name: "users" });
  const one = toggledCollapsed(new Set(), publicKey);
  assert.deepEqual([...one], [publicKey]);

  const both = setCollapsed(one, [salesKey], true);
  assert.equal(both.size, 2);
  assert.equal(setCollapsed(both, [publicKey], false).has(publicKey), false);
  assert.equal(setCollapsed(both, [publicKey], false).has(salesKey), true);
  assert.deepEqual([...toggledCollapsed(both, publicKey)], [salesKey]);
});
