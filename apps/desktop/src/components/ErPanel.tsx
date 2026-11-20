import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import type { NoticeAction, NoticeKind } from "../notifications";
import type {
  Candidate,
  ConnectionParams,
  Decision,
  DecisionAction,
  Endpoint,
  ErExportResult,
  ErPreview,
  ErProject,
  ErTableChoice,
  TableIdentity,
} from "../types";
import {
  decisionActionLabel,
  endpointLabel,
  parseErSvgLayout,
  relationKindClass,
  relationKindLabel,
  safeFileName,
  sameEndpoint,
  setCollapsed,
  tableKey,
  tableLabel,
  toggledCollapsed,
  withDecision,
  withoutDecision,
} from "../er";

const nonNullScope = "非空等值：两端全部键位非空，且以普通等号逐列比较时，目标至多匹配一行。";

interface Props {
  open: boolean;
  conn: ConnectionParams;
  database: string;
  databaseDisplay: string;
  /** 与文档导出共用的默认保存目录。 */
  defaultDir: string;
  selected: TableIdentity[];
  onClose: () => void;
  onNotify: (
    kind: NoticeKind,
    title: string,
    detail?: string,
    action?: NoticeAction,
  ) => void;
  onRevealPath: (path: string) => void;
  /** 导出成功后让主界面刷新「最近导出」。 */
  onExported: () => void;
}

function errText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

function toRef(table: TableIdentity): TableIdentity {
  return { schema: table.schema, name: table.name };
}

function parseColumns(value: string): string[] {
  return value
    .split(/[,，]/)
    .map((column) => column.trim())
    .filter(Boolean);
}

function suggestedColumns(choice: ErTableChoice | undefined): string {
  if (!choice) return "";
  const key = choice.keys[0] ?? choice.columns.slice(0, 1);
  return key.join(", ");
}

function defaultFilePath(dir: string, fileName: string): string {
  if (!dir || dir === ".") return fileName;
  const separator = dir.includes("\\") ? "\\" : "/";
  return dir.endsWith("/") || dir.endsWith("\\")
    ? `${dir}${fileName}`
    : `${dir}${separator}${fileName}`;
}

export function ErPanel({
  open,
  conn,
  database,
  databaseDisplay,
  defaultDir,
  selected,
  onClose,
  onNotify,
  onRevealPath,
  onExported,
}: Props) {
  const [catalogTables, setCatalogTables] = useState<ErTableChoice[]>([]);
  const [preview, setPreview] = useState<ErPreview | null>(null);
  const [svgUrl, setSvgUrl] = useState("");
  const [project, setProject] = useState<ErProject | null>(null);
  const [selectedKeys, setSelectedKeys] = useState<Set<string>>(new Set());
  const [collapsed, setCollapsedKeys] = useState<Set<string>>(new Set());
  const [showAll, setShowAll] = useState(true);
  const [originalSize, setOriginalSize] = useState(false);
  const [title, setTitle] = useState("");
  const [loading, setLoading] = useState(false);
  const [previewStale, setPreviewStale] = useState(false);
  const [tableFilter, setTableFilter] = useState("");
  const [saving, setSaving] = useState<"svg" | "project" | "load" | null>(null);
  const [deciding, setDeciding] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const [manualSource, setManualSource] = useState("");
  const [manualSourceColumns, setManualSourceColumns] = useState("");
  const [manualTarget, setManualTarget] = useState("");
  const [manualTargetColumns, setManualTargetColumns] = useState("");

  const seqRef = useRef(0);
  const revisionRef = useRef(0);
  const sessionRef = useRef<number | null>(null);
  const selectedKeysRef = useRef<Set<string>>(new Set());
  const collapsedRef = useRef<Set<string>>(new Set());
  const openRef = useRef(open);
  openRef.current = open;

  const applyPreview = useCallback((next: ErPreview) => {
    revisionRef.current = next.revision;
    sessionRef.current = next.session_id;
    setSvgUrl("");
    setCatalogTables(next.tables);
    setPreview(next);
    setProject(next.project);
    const keys = new Set(next.selected.map(tableKey));
    selectedKeysRef.current = keys;
    setSelectedKeys(keys);
    const folded = new Set((next.collapsed ?? []).map(tableKey));
    collapsedRef.current = folded;
    setCollapsedKeys(folded);
    setShowAll(next.show_all_columns);
    setTitle(next.title);
    setPreviewStale(false);
  }, []);

  // 打开时读取一次目录；关闭时让所有在途请求失效。
  useEffect(() => {
    if (!open) {
      seqRef.current += 1;
      sessionRef.current = null;
      setPreview(null);
      return;
    }
    const seq = ++seqRef.current;
    const requestId = ++revisionRef.current;
    sessionRef.current = null;
    setLoading(true);
    setPreviewStale(false);
    setPreview(null);
    setProject(null);
    setCatalogTables([]);
    const initialKeys = new Set(selected.map(tableKey));
    selectedKeysRef.current = initialKeys;
    setSelectedKeys(initialKeys);
    collapsedRef.current = new Set();
    setCollapsedKeys(new Set());
    setShowAll(true);
    setOriginalSize(false);
    setManualOpen(false);
    const nextTitle = `${databaseDisplay || database} ${conn.db_type === "mongodb" ? "集合关系图" : "数据库 ER 图"}`;
    setTitle(nextTitle);

    invoke<ErPreview>("er_open_cmd", {
      params: {
        conn,
        database,
        selected: selected.map(toRef),
        project: null,
        request_id: requestId,
        title: nextTitle,
        show_all_columns: true,
        collapsed: [],
      },
    })
      .then((next) => {
        if (seqRef.current !== seq) return;
        applyPreview(next);
      })
      .catch((err) => {
        if (seqRef.current !== seq) return;
        onNotify("error", "读取 ER 目录失败", errText(err));
      })
      .finally(() => {
        if (seqRef.current === seq) setLoading(false);
      });
    // 只在打开面板时读取目录；conn/database 变化时父组件会先关闭再重开。
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  // SVG 走 Blob URL：图很大时避免 data URL 过长，也能安全地隔离脚本。
  useEffect(() => {
    if (!preview) {
      setSvgUrl("");
      return;
    }
    const url = URL.createObjectURL(new Blob([preview.svg], { type: "image/svg+xml" }));
    setSvgUrl(url);
    return () => URL.revokeObjectURL(url);
  }, [preview]);

  const choices = catalogTables;
  const choiceMap = useMemo(() => {
    const map = new Map<string, ErTableChoice>();
    choices.forEach((choice) => map.set(tableKey(choice), choice));
    return map;
  }, [choices]);
  const selectedRefs = useMemo(
    () =>
      choices
        .filter((choice) => selectedKeys.has(tableKey(choice)))
        .map((choice) => toRef(choice)),
    [choices, selectedKeys],
  );
  const visibleChoices = useMemo(() => {
    const terms = tableFilter.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (terms.length === 0) return choices;
    return choices.filter((choice) => {
      const haystack = `${tableLabel(choice, database)}\n${choice.comment}`.toLowerCase();
      return terms.every((term) => haystack.includes(term));
    });
  }, [choices, tableFilter, database]);
  // 每次预览只解析一次卡片表头坐标，鼠标事件不再碰 SVG 文本。
  const svgLayout = useMemo(
    () => (preview ? parseErSvgLayout(preview.svg) : null),
    [preview],
  );
  const selectedKeyList = useMemo(
    () => selectedRefs.map((table) => tableKey(table)),
    [selectedRefs],
  );
  const foldedCount = selectedKeyList.filter((key) => collapsed.has(key)).length;

  const rerender = useCallback(
    async (
      nextSelected: TableIdentity[],
      nextProject: ErProject,
      nextShowAll: boolean,
      nextTitle: string,
      nextCollapsed: ReadonlySet<string>,
    ) => {
      if (!openRef.current) return;
      if (nextSelected.length === 0) {
        seqRef.current += 1;
        selectedKeysRef.current = new Set();
        setSelectedKeys(new Set());
        setPreview(null);
        setProject({ ...nextProject, selected: [] });
        setLoading(false);
        setPreviewStale(true);
        onNotify("error", "更新 ER 预览失败", "请至少选择一张表");
        return;
      }
      const session = sessionRef.current;
      if (session === null) return;
      const seq = ++seqRef.current;
      const revision = revisionRef.current + 1;
      revisionRef.current = revision;
      setLoading(true);
      setPreviewStale(true);
      try {
        const next = await invoke<ErPreview>("er_render_cmd", {
          params: {
            session_id: session,
            revision,
            selected: nextSelected.map(toRef),
            project: nextProject,
            show_all_columns: nextShowAll,
            collapsed: choices
              .filter((choice) => nextCollapsed.has(tableKey(choice)))
              .map(toRef),
            title: nextTitle,
          },
        });
        if (seqRef.current !== seq) return;
        applyPreview(next);
      } catch (err) {
        if (seqRef.current !== seq) return;
        onNotify("error", "更新 ER 预览失败", errText(err));
      } finally {
        if (seqRef.current === seq) setLoading(false);
      }
    },
    [applyPreview, choices, onNotify],
  );

  const updateSelection = useCallback(
    (nextKeys: Set<string>) => {
      if (!project) return;
      selectedKeysRef.current = nextKeys;
      setSelectedKeys(nextKeys);
      const nextSelected = choices
        .filter((choice) => nextKeys.has(tableKey(choice)))
        .map((choice) => toRef(choice));
      const nextProject: ErProject = { ...project, selected: nextSelected };
      setProject(nextProject);
      void rerender(nextSelected, nextProject, showAll, title, collapsedRef.current);
    },
    [choices, project, rerender, showAll, title],
  );

  const toggleChoice = (choice: ErTableChoice) => {
    const key = tableKey(choice);
    const next = new Set(selectedKeysRef.current);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    updateSelection(next);
  };

  const toggleAll = () => {
    const current = selectedKeysRef.current;
    const allSelected =
      choices.length > 0 && choices.every((choice) => current.has(tableKey(choice)));
    updateSelection(allSelected ? new Set() : new Set(choices.map(tableKey)));
  };

  const invertSelection = () => {
    const next = new Set<string>();
    choices.forEach((choice) => {
      const key = tableKey(choice);
      if (!selectedKeysRef.current.has(key)) next.add(key);
    });
    updateSelection(next);
  };

  const toggleShowAll = () => {
    const next = !showAll;
    setShowAll(next);
    if (project) void rerender(selectedRefs, project, next, title, collapsedRef.current);
  };

  /** 逐表状态以完整 TableId 识别；先更新本地集合，再按同一份快照重投影。 */
  const applyCollapsed = (next: Set<string>) => {
    collapsedRef.current = next;
    setCollapsedKeys(next);
    if (project) void rerender(selectedRefs, project, showAll, title, next);
  };

  const toggleTableCollapsed = (key: string) => {
    applyCollapsed(toggledCollapsed(collapsedRef.current, key));
  };

  const setAllCollapsed = (folded: boolean) => {
    const keys = selectedRefs.map(tableKey);
    applyCollapsed(setCollapsed(collapsedRef.current, keys, folded));
  };

  const commitTitle = () => {
    if (!project || !preview) return;
    const nextTitle = title.trim();
    if (!nextTitle) {
      setTitle(preview.title);
      return;
    }
    if (nextTitle === preview.title) return;
    void rerender(selectedRefs, project, showAll, nextTitle, collapsedRef.current);
  };

  const applyDecision = async (
    source: Endpoint,
    target: Endpoint,
    action: DecisionAction,
    failureTitle: string,
  ) => {
    if (!project || loading || deciding) return;
    const session = sessionRef.current;
    if (session === null) return;
    const seq = seqRef.current;
    setDeciding(true);
    try {
      const decision = await invoke<Decision>("er_decide_cmd", {
        params: { session_id: session, source, target, action },
      });
      if (!openRef.current || seqRef.current !== seq || sessionRef.current !== session) {
        return;
      }
      const next = withDecision(project, decision);
      setProject(next);
      setManualOpen(false);
      await rerender(selectedRefs, next, showAll, title, collapsedRef.current);
    } catch (err) {
      if (!openRef.current || seqRef.current !== seq || sessionRef.current !== session) return;
      onNotify("error", failureTitle, errText(err));
    } finally {
      setDeciding(false);
    }
  };

  const adopt = (candidate: Candidate) =>
    void applyDecision(candidate.source, candidate.target, "confirm", "采纳关系失败");
  const exclude = (candidate: Candidate) =>
    void applyDecision(candidate.source, candidate.target, "exclude", "排除关系失败");

  const undo = (decision: Decision) => {
    if (!project || loading || deciding) return;
    const next = withoutDecision(project, decision);
    setProject(next);
    void rerender(selectedRefs, next, showAll, title, collapsedRef.current);
  };

  const openManual = () => {
    if (choices.length === 0) return;
    const source =
      choices.find((choice) => selectedKeys.has(tableKey(choice))) ?? choices[0];
    const target = choices.find((choice) => tableKey(choice) !== tableKey(source)) ?? source;
    setManualSource(tableKey(source));
    setManualSourceColumns(suggestedColumns(source));
    setManualTarget(tableKey(target));
    setManualTargetColumns(suggestedColumns(target));
    setManualOpen(true);
  };

  const pickManualSource = (key: string) => {
    setManualSource(key);
    setManualSourceColumns(suggestedColumns(choiceMap.get(key)));
  };

  const pickManualTarget = (key: string) => {
    setManualTarget(key);
    setManualTargetColumns(suggestedColumns(choiceMap.get(key)));
  };

  const submitManual = () => {
    const source = choiceMap.get(manualSource);
    const target = choiceMap.get(manualTarget);
    if (!source || !target) {
      onNotify("error", "确认手工关系失败", "请选择关系的来源表和目标表");
      return;
    }
    const sourceColumns = parseColumns(manualSourceColumns);
    const targetColumns = parseColumns(manualTargetColumns);
    if (sourceColumns.length === 0 || targetColumns.length === 0) {
      onNotify("error", "确认手工关系失败", "请填写关系两端的字段，按顺序用逗号分隔");
      return;
    }
    void applyDecision(
      { table: toRef(source), columns: sourceColumns },
      { table: toRef(target), columns: targetColumns },
      "confirm",
      "确认手工关系失败",
    );
  };

  const exportSvg = async () => {
    if (!preview || !project || loading || saving) return;
    const defaultName = `${safeFileName(title || databaseDisplay, "er")}.svg`;
    try {
      const path = await saveDialog({
        defaultPath: defaultFilePath(defaultDir, defaultName),
        filters: [{ name: "SVG 矢量图", extensions: ["svg"] }],
      });
      if (typeof path !== "string" || !path || !openRef.current) return;
      setSaving("svg");
      const result = await invoke<ErExportResult>("er_export_cmd", {
        params: {
          session_id: preview.session_id,
          revision: preview.revision,
          selected: selectedRefs,
          project,
          out_path: path,
          title,
          show_all_columns: showAll,
          collapsed: choices
            .filter((choice) => collapsedRef.current.has(tableKey(choice)))
            .map(toRef),
        },
      });
      const detail = [
        result.out_path,
        `表 ${result.table_count} · 关系 ${result.relation_count} · 候选 ${result.candidate_count} · 跨范围候选 ${result.external_candidate_count} · 图外引用 ${result.external_count}`,
        ...(result.warnings.length > 0
          ? [
              `提示：${result.warnings.slice(0, 5).join("；")}${
                result.warnings.length > 5 ? ` 等 ${result.warnings.length} 条` : ""
              }`,
            ]
          : []),
      ].join("\n");
      onNotify("success", "ER 图已导出", detail, {
        label: "打开所在文件夹",
        onClick: () => onRevealPath(result.out_path),
      });
      onExported();
    } catch (err) {
      if (openRef.current) onNotify("error", "导出 ER 图失败", errText(err));
    } finally {
      setSaving(null);
    }
  };

  const saveProject = async () => {
    if (!project || saving || loading || deciding || selectedRefs.length === 0) return;
    const defaultName = `${safeFileName(databaseDisplay || database, "er")}.er.json`;
    try {
      const path = await saveDialog({
        defaultPath: defaultFilePath(defaultDir, defaultName),
        filters: [{ name: "ER 项目 JSON", extensions: ["json"] }],
      });
      if (typeof path !== "string" || !path || !openRef.current) return;
      setSaving("project");
      const saved = await invoke<ErProject>("er_write_project_cmd", {
        params: { conn, database, path, project },
      });
      setProject(saved);
      onNotify("success", "项目修正已保存", path);
    } catch (err) {
      if (openRef.current) onNotify("error", "保存项目 JSON 失败", errText(err));
    } finally {
      setSaving(null);
    }
  };

  const reloadCatalog = async () => {
    if (!project || saving || loading || deciding || selectedRefs.length === 0) return;
    const seq = ++seqRef.current;
    const requestId = ++revisionRef.current;
    setLoading(true);
    try {
      const next = await invoke<ErPreview>("er_open_cmd", {
        params: {
          conn,
          database,
          selected: selectedRefs,
          project,
          request_id: requestId,
          title,
          show_all_columns: showAll,
          collapsed: choices
            .filter((choice) => collapsedRef.current.has(tableKey(choice)))
            .map(toRef),
        },
      });
      if (seqRef.current !== seq) return;
      applyPreview(next);
    } catch (err) {
      if (seqRef.current !== seq) return;
      onNotify("error", "重新读取目录失败", errText(err));
    } finally {
      if (seqRef.current === seq) setLoading(false);
    }
  };

  const loadProject = async () => {
    if (!project || saving || loading || deciding) return;
    try {
      const path = await openDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "ER 项目 JSON", extensions: ["json"] }],
      });
      if (typeof path !== "string" || !path || !openRef.current) return;
      setSaving("load");
      const loaded = await invoke<ErProject>("er_read_project_cmd", {
        params: { conn, database, path },
      });
      if (!openRef.current) return;
      const known = new Set(choices.map(tableKey));
      const wantedTables = loaded.selected;
      const wanted = (wantedTables ?? []).filter((table) => known.has(tableKey(table)));
      const missing = (wantedTables ?? []).filter((table) => !known.has(tableKey(table)));
      // 与 CLI 一致：selected 为 null 表示全库；空数组表示没有勾选。
      const nextSelected =
        wantedTables === null
          ? choices.map((choice) => toRef(choice))
          : wanted.length > 0
            ? choices
                .filter((choice) => wanted.some((table) => tableKey(table) === tableKey(choice)))
                .map((choice) => toRef(choice))
            : wantedTables.length === 0
              ? []
              : selectedRefs;
      if (missing.length > 0) {
        onNotify(
          "info",
          "项目中的部分表已不存在",
          `${missing.map((table) => tableLabel(table, database)).join("、")}；已保留其余范围。`,
        );
      }
      if (wantedTables !== null && wantedTables.length > 0 && wanted.length === 0) {
        onNotify("info", "项目中的表都不在当前目录", "已保留当前勾选范围。");
      }
      const nextProject: ErProject = { ...loaded, selected: nextSelected };
      const nextKeys = new Set(nextSelected.map(tableKey));
      selectedKeysRef.current = nextKeys;
      setSelectedKeys(nextKeys);
      setProject(nextProject);
      await rerender(nextSelected, nextProject, showAll, title, collapsedRef.current);
    } catch (err) {
      if (openRef.current) onNotify("error", "载入项目失败", errText(err));
    } finally {
      setSaving(null);
    }
  };

  const manualSourceChoice = choiceMap.get(manualSource);
  const manualTargetChoice = choiceMap.get(manualTarget);
  const canExport =
    !!preview &&
    !!project &&
    selectedRefs.length > 0 &&
    !loading &&
    !previewStale &&
    !saving &&
    !deciding;
  const canInteract = !!preview && !!project && !loading && !deciding;

  if (!open) return null;
  const entity = conn.db_type === "mongodb" ? "集合" : "表";
  const panelTitle = conn.db_type === "mongodb" ? "集合关系图预览" : "ER 图预览";

  return (
    <div
      className="er-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={panelTitle}
      onClick={onClose}
    >
      <section className="er-shell" onClick={(event) => event.stopPropagation()}>
        <div className="er-head">
          <div>
            <h3>{panelTitle}</h3>
            <p className="muted">{preview?.scope_label || database}</p>
          </div>
          <div className="er-head-meta">
            {preview && (
              <>
                <span className="er-chip">{conn.db_type === "mongodb" ? "集合" : "表"} {preview.diagram.tables.length}</span>
                <span className="er-chip">关系 {preview.diagram.relations.length}</span>
                <span className="er-chip">候选 {preview.diagram.candidates.length}</span>
                {preview.diagram.external_candidate_count > 0 && (
                  <span className="er-chip">
                    跨范围候选 {preview.diagram.external_candidate_count}
                  </span>
                )}
                <span className="er-chip">图外引用 {preview.diagram.external_count}</span>
              </>
            )}
            <button className="small" onClick={onClose}>
              关闭
            </button>
          </div>
        </div>

        <div className="er-toolbar">
          <div className="field grow">
            <label>图标题</label>
            <input
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              onBlur={commitTitle}
              disabled={!project || !preview || loading || deciding}
              onKeyDown={(event) => {
                if (event.key === "Enter") event.currentTarget.blur();
              }}
            />
          </div>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={showAll}
              onChange={toggleShowAll}
              disabled={!project || loading || deciding || selectedRefs.length === 0}
            />
            显示全部字段
          </label>
          <div className="er-fold-actions" role="group" aria-label="字段展开">
            <button
              className="small"
              onClick={() => setAllCollapsed(true)}
              disabled={!canInteract || foldedCount === selectedKeyList.length}
              title="收起所有已选表的字段，只保留表头"
            >
              全部收起
            </button>
            <button
              className="small"
              onClick={() => setAllCollapsed(false)}
              disabled={!canInteract || foldedCount === 0}
              title="展开所有已选表的字段"
            >
              全部展开
            </button>
            <span className="muted" aria-live="polite">
              已收起 {foldedCount}
            </span>
          </div>
          <button
            className="small"
            aria-pressed={originalSize}
            onClick={() => setOriginalSize(!originalSize)}
            disabled={!preview}
            title="也可点击图内空白处切换"
          >
            {originalSize ? "适应宽度" : "原始尺寸"}
          </button>
          {loading && (
            <span className="muted">
              <span className="spinner" />
              正在更新预览…
            </span>
          )}
          {!loading && previewStale && preview && (
            <span className="muted">预览未更新，暂时不能导出</span>
          )}
        </div>

        <div className="er-body">
          <aside className="er-side">
            <div className="er-side-head">
              <strong>成图范围</strong>
              <span className="muted">
                已选 {selectedRefs.length}/{choices.length}
              </span>
            </div>
            <div className="bulk">
              <button
                onClick={toggleAll}
                disabled={choices.length === 0 || loading || deciding}
              >
                {selectedRefs.length === choices.length ? "全不选" : "全选"}
              </button>
              <button
                onClick={invertSelection}
                disabled={choices.length === 0 || loading || deciding}
              >
                反选
              </button>
            </div>
            <input
              className="search"
              placeholder={`搜索${entity}名或注释…`}
              value={tableFilter}
              onChange={(event) => setTableFilter(event.target.value)}
            />
            <ul className="er-table-list">
              {visibleChoices.length === 0 && (
                <li>
                  <span className="muted">没有匹配的{entity}</span>
                </li>
              )}
              {visibleChoices.map((choice) => {
                const key = tableKey(choice);
                return (
                  <li key={key} className={selectedKeys.has(key) ? "selected" : ""}>
                    <label className="checkbox">
                      <input
                        type="checkbox"
                        checked={selectedKeys.has(key)}
                        onChange={() => toggleChoice(choice)}
                        disabled={loading || deciding}
                      />
                      <span className="mono">{tableLabel(choice, database)}</span>
                    </label>
                    <span className="er-table-comment" title={choice.comment}>
                      {choice.comment || "（无注释）"}
                    </span>
                    {selectedKeys.has(key) && (
                      <button
                        type="button"
                        className="small er-row-fold"
                        aria-expanded={!collapsed.has(key)}
                        disabled={loading || deciding}
                        onClick={() => toggleTableCollapsed(key)}
                      >
                        {collapsed.has(key) ? "展开字段" : "收起字段"}
                      </button>
                    )}
                  </li>
                );
              })}
            </ul>
          </aside>

          <main className="er-content">
            <div className="er-canvas">
              {loading && !preview ? (
                <div className="empty">
                  <span>
                    <span className="spinner" />
                    正在读取目录并推断关系…
                  </span>
                </div>
              ) : preview && svgUrl ? (
                <div
                  className={originalSize ? "er-figure original" : "er-figure"}
                  style={
                    svgLayout && svgLayout.width > 0
                      ? {
                          aspectRatio: `${svgLayout.width} / ${svgLayout.height}`,
                          width: originalSize ? `${svgLayout.width}px` : "100%",
                        }
                      : undefined
                  }
                >
                  <button
                    type="button"
                    className="er-zoom-control"
                    aria-label={originalSize ? "适应预览宽度" : "按原始尺寸查看"}
                    aria-pressed={originalSize}
                    onClick={() => setOriginalSize(!originalSize)}
                  >
                    <img className="er-svg" alt={panelTitle} src={svgUrl} />
                  </button>
                  {svgLayout?.headers.map((header) => {
                    const key = tableKey(header);
                    const folded = collapsed.has(key);
                    const label = tableLabel(header, database);
                    return (
                      <button
                        key={key}
                        type="button"
                        className={folded ? "er-hit collapsed" : "er-hit"}
                        style={{
                          left: `${(header.x / svgLayout.width) * 100}%`,
                          top: `${(header.y / svgLayout.height) * 100}%`,
                          width: `${(header.width / svgLayout.width) * 100}%`,
                          height: `${(header.height / svgLayout.height) * 100}%`,
                        }}
                        aria-expanded={!folded}
                        aria-label={`${folded ? "展开" : "收起"} ${label} 的字段`}
                        title={`${folded ? "展开" : "收起"} ${label} 字段`}
                        onClick={(event) => {
                          event.stopPropagation();
                          toggleTableCollapsed(key);
                        }}
                      >
                        <span aria-hidden="true">{folded ? "＋" : "－"}</span>
                      </button>
                    );
                  })}
                </div>
              ) : (
                <div className="empty">勾选{entity}后生成关系图</div>
              )}
            </div>

            {preview && (
              <div className="er-sections">
                <section className="er-card">
                  <h4>
                    关系 <span className="muted">（{preview.diagram.relations.length}）</span>
                  </h4>
                  {preview.diagram.relations.length === 0 ? (
                    <p className="muted">当前范围内没有可确认的关系。</p>
                  ) : (
                    <ul className="er-list">
                      {preview.diagram.relations.map((relation, index) => (
                        <li key={`r-${index}`}>
                          <div className="er-endpoints">
                            <span className="mono">
                              {endpointLabel(relation.source, database)}
                            </span>
                            <span className="er-arrow">→</span>
                            <span className="mono">
                              {endpointLabel(relation.target, database)}
                            </span>
                          </div>
                          <div className="er-row">
                            <span
                              className={`er-badge ${relationKindClass(relation.kind)}`}
                            >
                              {relationKindLabel(relation.kind)}
                            </span>
                            {(relation.kind === "inferred" || relation.kind === "confirmed") && (
                              <button
                                className="small"
                                disabled={!canInteract}
                                onClick={() =>
                                  void applyDecision(
                                    relation.source,
                                    relation.target,
                                    "exclude",
                                    "排除关系失败",
                                  )
                                }
                              >
                                排除此关系
                              </button>
                            )}
                          </div>
                          <details>
                            <summary>{relation.kind === "lookup" ? "查看视图依据" : "查看关系依据"}</summary>
                            <p>{relation.reason}</p>
                          </details>
                          {relation.condition?.non_null_equality && <p>{nonNullScope}</p>}
                        </li>
                      ))}
                    </ul>
                  )}
                </section>

                <section className="er-card">
                  <h4>
                    待判断候选 <span className="muted">（{preview.diagram.candidates.length}）</span>
                  </h4>
                  {preview.diagram.candidates.length === 0 ? (
                    <p className="muted">没有保留的竞争候选。</p>
                  ) : (
                    <ul className="er-list">
                      {preview.diagram.candidates.map((candidate, index) => {
                        const decided = project?.decisions.find(
                          (decision) =>
                            sameEndpoint(decision.source, candidate.source) &&
                            sameEndpoint(decision.target, candidate.target),
                        );
                        return (
                          <li key={`c-${index}`}>
                            <div className="er-endpoints">
                              <span className="mono">
                                {endpointLabel(candidate.source, database)}
                              </span>
                              <span className="er-arrow">→</span>
                              <span className="mono">
                                {endpointLabel(candidate.target, database)}
                              </span>
                            </div>
                            <details>
                              <summary>查看候选依据</summary>
                              <p>{candidate.reason}</p>
                              {candidate.condition?.required_target.map(({column, value}) => (
                                <p key={column}>目标索引条件：{column} = {JSON.stringify(value)}（业务适用性待核对）</p>
                              ))}
                              {!!candidate.condition?.unbound_target_columns.length && (
                                <p>尚未绑定的目标键位：{candidate.condition.unbound_target_columns.join("、")}</p>
                              )}
                            </details>
                            {candidate.condition?.non_null_equality && <p>{nonNullScope}</p>}
                            <div className="er-row">
                              <button
                                className="small primary"
                                disabled={!canInteract || !!candidate.condition}
                                title={candidate.condition ? "条件尚未被证实，不能采纳为无条件关系" : undefined}
                                onClick={() => adopt(candidate)}
                              >
                                采纳
                              </button>
                              <button
                                className="small"
                                disabled={!canInteract}
                                onClick={() => exclude(candidate)}
                              >
                                排除
                              </button>
                              {decided && (
                                <span className="muted">
                                  已{decisionActionLabel(decided.action)}
                                </span>
                              )}
                            </div>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </section>

                <section className="er-card">
                  <div className="er-card-head">
                    <h4>
                      人工修正{" "}
                      <span className="muted">（{project?.decisions.length ?? 0}）</span>
                    </h4>
                    <button
                      className="small"
                      onClick={openManual}
                      disabled={!canInteract || choices.length === 0}
                    >
                      补充关系
                    </button>
                  </div>

                  {manualOpen && (
                    <div className="er-manual">
                      <div className="row">
                        <div className="field grow">
                          <label>来源{entity}</label>
                          <select
                            value={manualSource}
                            onChange={(event) => pickManualSource(event.target.value)}
                            disabled={deciding}
                          >
                            {choices.map((choice) => (
                              <option key={tableKey(choice)} value={tableKey(choice)}>
                                {tableLabel(choice, database)}
                              </option>
                            ))}
                          </select>
                        </div>
                        <div className="field grow">
                          <label>目标{entity}</label>
                          <select
                            value={manualTarget}
                            onChange={(event) => pickManualTarget(event.target.value)}
                            disabled={deciding}
                          >
                            {choices.map((choice) => (
                              <option key={tableKey(choice)} value={tableKey(choice)}>
                                {tableLabel(choice, database)}
                              </option>
                            ))}
                          </select>
                        </div>
                      </div>
                      <div className="row">
                        <div className="field grow">
                          <label>来源字段（按顺序，逗号分隔）</label>
                          <input
                            value={manualSourceColumns}
                            onChange={(event) =>
                              setManualSourceColumns(event.target.value)
                            }
                            disabled={deciding}
                          />
                        </div>
                        <div className="field grow">
                          <label>目标字段（完整唯一键）</label>
                          <input
                            value={manualTargetColumns}
                            onChange={(event) =>
                              setManualTargetColumns(event.target.value)
                            }
                            disabled={deciding}
                          />
                        </div>
                      </div>
                      <div className="er-hints">
                        <span className="muted">来源可选字段：</span>
                        {(manualSourceChoice?.columns ?? []).map((column) => (
                          <button
                            key={column}
                            className="er-column"
                            onClick={() => setManualSourceColumns(column)}
                          >
                            {column}
                          </button>
                        ))}
                      </div>
                      <div className="er-hints">
                        <span className="muted">来源唯一键：</span>
                        {(manualSourceChoice?.keys ?? []).map((key, index) => (
                          <button
                            key={`${index}-${key.join(",")}`}
                            className="er-column"
                            onClick={() => setManualSourceColumns(key.join(", "))}
                          >
                            {key.join(", ")}
                          </button>
                        ))}
                      </div>
                      <div className="er-hints">
                        <span className="muted">目标唯一键：</span>
                        {(manualTargetChoice?.keys ?? []).map((key, index) => (
                          <button
                            key={`${index}-${key.join(",")}`}
                            className="er-column"
                            onClick={() => setManualTargetColumns(key.join(", "))}
                          >
                            {key.join(", ")}
                          </button>
                        ))}
                      </div>
                      {manualTargetChoice && manualTargetChoice.keys.length === 0 && (
                        <p className="muted">
                          目标{entity}没有可验证的完整唯一键，无法确认这条记录引用。
                        </p>
                      )}
                      <div className="er-row">
                        <button
                          className="small primary"
                          onClick={submitManual}
                          disabled={!canInteract || deciding}
                        >
                          确认并加入
                        </button>
                        <button className="small" onClick={() => setManualOpen(false)}>
                          取消
                        </button>
                      </div>
                    </div>
                  )}

                  {(project?.decisions.length ?? 0) === 0 ? (
                    <p className="muted">还没有人工修正。</p>
                  ) : (
                    <ul className="er-list">
                      {project?.decisions.map((decision, index) => (
                        <li key={`d-${index}`}>
                          <div className="er-endpoints">
                            <span className="mono">
                              {endpointLabel(decision.source, database)}
                            </span>
                            <span className="er-arrow">→</span>
                            <span className="mono">
                              {endpointLabel(decision.target, database)}
                            </span>
                          </div>
                          <div className="er-row">
                            <span
                              className={`er-badge ${
                                decision.action === "confirm"
                                  ? "kind-confirmed"
                                  : "kind-excluded"
                              }`}
                            >
                              {decisionActionLabel(decision.action)}
                            </span>
                            <button
                              className="small"
                              onClick={() => undo(decision)}
                              disabled={!canInteract}
                            >
                              撤销
                            </button>
                          </div>
                        </li>
                      ))}
                    </ul>
                  )}
                </section>

                <section className="er-card">
                  <h4>读取范围与提示</h4>
                  <p className="muted">
                    范围：{preview.scope_label}
                    <br />
                    图外引用：{preview.diagram.external_count} 条；仅调整勾选不会重新读取数据库。
                  </p>
                  {preview.diagram.warnings.length === 0 ? (
                    <p className="muted">没有需要复核的提示。</p>
                  ) : (
                    <ul className="er-warnings">
                      {preview.diagram.warnings.map((warning, index) => (
                        <li key={`w-${index}`}>{warning}</li>
                      ))}
                    </ul>
                  )}
                </section>
              </div>
            )}
          </main>
        </div>

        <div className="er-foot">
          <span className="muted">
            预览与 SVG 使用同一份裁决；勾选、字段范围或修正变化后需等预览更新完成。
          </span>
          <div className="row">
            <button
              onClick={reloadCatalog}
              disabled={
                !project ||
                !!saving ||
                loading ||
                deciding ||
                selectedRefs.length === 0
              }
              title="重新读取键、类型与注释；保留当前勾选和人工修正"
            >
              重新读取目录
            </button>
            <button
              onClick={loadProject}
              disabled={!project || !!saving || loading || deciding}
            >
              {saving === "load" ? "载入中…" : "载入项目 JSON"}
            </button>
            <button
              onClick={saveProject}
              disabled={
                !project || !!saving || loading || deciding || selectedRefs.length === 0
              }
            >
              {saving === "project" ? "保存中…" : "保存项目 JSON"}
            </button>
            <button
              className="primary"
              onClick={exportSvg}
              disabled={!canExport}
              title={
                !preview
                  ? "请先勾选表并生成预览"
                  : loading
                    ? "预览正在更新"
                    : previewStale
                      ? "预览未更新，请等待刷新后再导出"
                      : undefined
              }
            >
              {saving === "svg" ? "保存中…" : "保存 SVG"}
            </button>
          </div>
        </div>
      </section>
    </div>
  );
}
