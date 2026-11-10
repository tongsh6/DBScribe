import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { ConnectionManager } from "./components/ConnectionManager";
import { TableDetail } from "./components/TableDetail";
import type {
  AppState,
  ColumnView,
  ConnectionParams,
  ExportResult,
  HistoryEntry,
  SavedConnection,
  TableSummary,
} from "./types";
import "./App.css";

const FORMATS = [
  { value: "excel", label: "Excel (.xlsx)", ext: "xlsx" },
  { value: "markdown", label: "Markdown (.md)", ext: "md" },
  { value: "html", label: "HTML (.html)", ext: "html" },
  { value: "json", label: "JSON (.json)", ext: "json" },
  { value: "xml", label: "XML (.xml)", ext: "xml" },
];

function errText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

function App() {
  const [connections, setConnections] = useState<SavedConnection[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [managerOpen, setManagerOpen] = useState(false);
  const [remember, setRemember] = useState(true);
  const [rememberPassword, setRememberPassword] = useState(true);

  const [conn, setConn] = useState<ConnectionParams>({
    host: "127.0.0.1",
    port: 3306,
    user: "root",
    password: "",
  });

  const [databases, setDatabases] = useState<string[]>([]);
  const [database, setDatabase] = useState("");
  const [tables, setTables] = useState<TableSummary[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");
  const [sortKey, setSortKey] = useState<"name" | "comment" | "columns">("name");
  const [sortAsc, setSortAsc] = useState(true);
  const [format, setFormat] = useState("excel");
  const [sheetMode, setSheetMode] = useState("multi");
  const [title, setTitle] = useState("");
  const [outDir, setOutDir] = useState(".");
  const [busy, setBusy] = useState<string | null>(null);
  const [exportProgress, setExportProgress] = useState<{ done: number; total: number } | null>(null);
  const [error, setError] = useState("");
  const [result, setResult] = useState<ExportResult | null>(null);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [prefsReady, setPrefsReady] = useState(false);

  // 表结构预览
  const [previewTable, setPreviewTable] = useState<TableSummary | null>(null);
  const [previewColumns, setPreviewColumns] = useState<ColumnView[] | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState("");

  const formatMeta = FORMATS.find((f) => f.value === format) ?? FORMATS[0];
  const activeConnection = useMemo(
    () => connections.find((c) => c.id === activeId) ?? null,
    [connections, activeId],
  );
  const connected = databases.length > 0;
  const searchRef = useRef<HTMLInputElement>(null);

  // 键盘快捷键：Ctrl/Cmd+F 聚焦搜索，Esc 关闭弹窗与抽屉
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      } else if (e.key === "Escape") {
        setPreviewTable(null);
        setManagerOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // 监听后端导出进度事件
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<{ done: number; total: number }>("export-progress", (e) => {
      setExportProgress(e.payload);
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      unlisten?.();
    };
  }, []);

  const refreshState = useCallback(async (): Promise<AppState> => {
    const state = await invoke<AppState>("load_app_state");
    setConnections(state.connections ?? []);
    setActiveId(state.active_connection ?? null);
    setHistory(state.history ?? []);
    return state;
  }, []);

  const applyConnection = useCallback((c: SavedConnection) => {
    setActiveId(c.id);
    setConn({ host: c.host, port: c.port, user: c.user, password: "" });
    setDatabases([]);
    setDatabase("");
    setTables([]);
    setSelected(new Set());
    setResult(null);
    setError("");

    // 已保存密码的连接：自动从系统凭据管理器取回，免去重复输入
    if (c.has_password) {
      invoke<string | null>("get_connection_password", { id: c.id })
        .then((pwd) => {
          if (pwd) setConn((prev) => ({ ...prev, password: pwd }));
        })
        .catch(() => undefined);
    }
  }, []);

  useEffect(() => {
    refreshState()
      .then((state) => {
        const active =
          state.connections.find((c) => c.id === state.active_connection) ?? state.connections[0];
        if (active) applyConnection(active);

        // 恢复上次使用的导出设置
        const p = state.preferences;
        if (p) {
          if (p.format) setFormat(p.format);
          if (p.sheet_mode) setSheetMode(p.sheet_mode);
          if (p.out_dir) setOutDir(p.out_dir);
        }
        setPrefsReady(true);
      })
      .catch(() => undefined);
  }, [refreshState, applyConnection]);

  // 导出设置变化后防抖保存（下次启动直接沿用）
  useEffect(() => {
    if (!prefsReady) return;
    const timer = setTimeout(() => {
      invoke("save_preferences", {
        preferences: { format, sheet_mode: sheetMode, out_dir: outDir },
      }).catch(() => undefined);
    }, 600);
    return () => clearTimeout(timer);
  }, [format, sheetMode, outDir, prefsReady]);

  useEffect(() => {
    if (database) setTitle(`${database}数据库设计文档`);
  }, [database]);

  const selectConnection = async (id: string) => {
    if (!id) return;
    const target = connections.find((c) => c.id === id);
    if (!target) return;
    applyConnection(target);
    await invoke("set_active_connection", { id }).catch(() => undefined);
  };

  const rememberConnection = useCallback(async () => {
    if (!remember) return;
    try {
      await invoke<SavedConnection>("save_connection", {
        conn: {
          id: activeConnection?.id ?? "",
          name:
            activeConnection?.name?.trim() ||
            `${conn.user}@${conn.host}${conn.port !== 3306 ? `:${conn.port}` : ""}`,
          host: conn.host,
          port: conn.port,
          user: conn.user,
        },
        // 记住密码：交给系统凭据管理器保存
        password: rememberPassword && conn.password ? conn.password : null,
      });
      await refreshState();
    } catch {
      /* 记住连接失败不影响主流程 */
    }
  }, [remember, rememberPassword, activeConnection, conn, refreshState]);

  const testConnection = useCallback(async () => {
    setBusy("正在连接数据库…");
    setError("");
    setResult(null);
    try {
      const msg = await invoke<string>("test_connection", { conn });
      const list = await invoke<string[]>("list_databases_cmd", { conn });
      setDatabases(list);
      setDatabase((prev) => (prev && list.includes(prev) ? prev : list[0] || ""));
      await rememberConnection();
      alert(msg);
    } catch (e) {
      setDatabases([]);
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  }, [conn, rememberConnection]);

  const loadTables = useCallback(async () => {
    if (!database) return;
    setBusy("正在读取表结构…");
    setError("");
    setResult(null);
    try {
      const list = await invoke<TableSummary[]>("list_tables_cmd", { conn, database });
      setTables(list);
      setSelected(new Set(list.map((t) => t.name)));
      await rememberConnection();
    } catch (e) {
      setTables([]);
      setSelected(new Set());
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  }, [conn, database, rememberConnection]);

  const openPreview = useCallback(
    async (table: TableSummary) => {
      setPreviewTable(table);
      setPreviewColumns(null);
      setPreviewError("");
      setPreviewLoading(true);
      try {
        const columns = await invoke<ColumnView[]>("get_table_columns_cmd", {
          conn,
          database,
          table: table.name,
        });
        setPreviewColumns(columns);
      } catch (e) {
        setPreviewError(errText(e));
      } finally {
        setPreviewLoading(false);
      }
    },
    [conn, database],
  );

  const visibleTables = useMemo(() => {
    // 搜索支持多个关键词（空格分隔），需全部命中
    const terms = filter.trim().toLowerCase().split(/\s+/).filter(Boolean);
    let list = tables;
    if (terms.length > 0) {
      list = list.filter((t) => {
        const haystack = `${t.name}\n${t.comment}`.toLowerCase();
        return terms.every((term) => haystack.includes(term));
      });
    }
    const dir = sortAsc ? 1 : -1;
    return [...list].sort((a, b) => {
      switch (sortKey) {
        case "columns":
          return (a.column_count - b.column_count) * dir || a.name.localeCompare(b.name);
        case "comment":
          return a.comment.localeCompare(b.comment, "zh") * dir || a.name.localeCompare(b.name);
        default:
          return a.name.localeCompare(b.name) * dir;
      }
    });
  }, [tables, filter, sortKey, sortAsc]);

  const toggleSort = (key: "name" | "comment" | "columns") => {
    if (sortKey === key) setSortAsc(!sortAsc);
    else {
      setSortKey(key);
      setSortAsc(true);
    }
  };

  const sortMark = (key: "name" | "comment" | "columns") =>
    sortKey === key ? (sortAsc ? " ▲" : " ▼") : "";

  const selectedCount = useMemo(
    () => tables.reduce((n, t) => (selected.has(t.name) ? n + 1 : n), 0),
    [tables, selected],
  );
  const totalColumns = useMemo(
    () => tables.reduce((sum, t) => sum + t.column_count, 0),
    [tables],
  );

  const toggleTable = (name: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const selectAllVisible = () => {
    setSelected((prev) => {
      const next = new Set(prev);
      visibleTables.forEach((t) => next.add(t.name));
      return next;
    });
  };
  const clearAll = () => setSelected(new Set());
  const invertVisible = () => {
    setSelected((prev) => {
      const next = new Set(prev);
      visibleTables.forEach((t) => {
        if (next.has(t.name)) next.delete(t.name);
        else next.add(t.name);
      });
      return next;
    });
  };

  const chooseDirectory = useCallback(async () => {
    try {
      const dir = await openDialog({ directory: true, multiple: false, defaultPath: outDir });
      if (typeof dir === "string" && dir) setOutDir(dir);
    } catch (e) {
      setError(errText(e));
    }
  }, [outDir]);

  const doExport = useCallback(async () => {
    if (!database) {
      setError("请先选择数据库");
      return;
    }
    if (selectedCount === 0) {
      setError("请至少勾选一张表");
      return;
    }
    const fileName = `${(title || `${database}数据库设计文档`).replace(/[\\/:*?"<>|]/g, "_")}.${formatMeta.ext}`;
    const sep = outDir.includes("\\") ? "\\" : "/";
    const outPath =
      outDir.endsWith("/") || outDir.endsWith("\\")
        ? `${outDir}${fileName}`
        : `${outDir}${sep}${fileName}`;

    const chosen = tables.filter((t) => selected.has(t.name)).map((t) => t.name);
    const tablesParam = chosen.length === tables.length ? null : chosen;

    setBusy("正在导出文档…");
    setError("");
    setResult(null);
    setExportProgress({ done: 0, total: selectedCount });
    try {
      const res = await invoke<ExportResult>("export_document", {
        conn,
        params: {
          database,
          title,
          format,
          sheet_mode: sheetMode,
          out_path: outPath,
          tables: tablesParam,
        },
      });
      setResult(res);
      refreshState()
        .then((s) => setHistory(s.history ?? []))
        .catch(() => undefined);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(null);
      setExportProgress(null);
    }
  }, [
    conn,
    database,
    title,
    format,
    sheetMode,
    outDir,
    formatMeta.ext,
    tables,
    selected,
    selectedCount,
    refreshState,
  ]);

  const revealPath = useCallback(async (path: string) => {
    try {
      await revealItemInDir(path);
    } catch (e) {
      setError(errText(e));
    }
  }, []);

  const clearHistory = useCallback(async () => {
    try {
      await invoke("clear_history");
      setHistory([]);
    } catch (e) {
      setError(errText(e));
    }
  }, []);

  return (
    <div className="app">
      <header>
        <div className="brand">
          <span className="logo">
            <svg viewBox="0 0 1024 1024" width="22" height="22" aria-hidden="true">
              <g fill="none" stroke="#fff" strokeWidth="72" strokeLinecap="round">
                <ellipse cx="512" cy="330" rx="226" ry="90" />
                <path d="M286 330v352c0 49.7 101.2 90 226 90s226-40.3 226-90V330" />
                <path d="M286 466c0 49.7 101.2 90 226 90s226-40.3 226-90" />
                <path d="M286 602c0 49.7 101.2 90 226 90s226-40.3 226-90" />
              </g>
            </svg>
          </span>
          <div>
            <h1>DBScribe</h1>
            <p>跨平台数据库结构文档工具</p>
          </div>
        </div>
        <div className="status">
          <span className={`dot ${connected ? "on" : ""}`} />
          <span className="muted">
            {connected
              ? `${activeConnection?.name ?? conn.user + "@" + conn.host}`
              : "未连接"}
          </span>
        </div>
      </header>

      <main>
        {/* 左栏：连接与数据库 */}
        <section className="panel left">
          <div className="card">
            <div className="card-head">
              <h2>连接</h2>
              <button className="small" onClick={() => setManagerOpen(true)}>
                管理
              </button>
            </div>
            <select
              value={activeId ?? ""}
              onChange={(e) => selectConnection(e.target.value)}
              disabled={connections.length === 0}
            >
              {connections.length === 0 && <option value="">未保存的连接</option>}
              {connections.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>

            <div className="row">
              <div className="field grow">
                <label>主机</label>
                <input
                  value={conn.host}
                  onChange={(e) => setConn({ ...conn, host: e.target.value })}
                />
              </div>
              <div className="field" style={{ width: 92 }}>
                <label>端口</label>
                <input
                  type="number"
                  value={conn.port}
                  onChange={(e) => setConn({ ...conn, port: Number(e.target.value) || 3306 })}
                />
              </div>
            </div>
            <div className="row">
              <div className="field grow">
                <label>用户</label>
                <input
                  value={conn.user}
                  onChange={(e) => setConn({ ...conn, user: e.target.value })}
                />
              </div>
              <div className="field grow">
                <label>密码</label>
                <input
                  type="password"
                  value={conn.password}
                  placeholder="不保存"
                  onChange={(e) => setConn({ ...conn, password: e.target.value })}
                />
              </div>
            </div>
            <label className="checkbox">
              <input
                type="checkbox"
                checked={remember}
                onChange={(e) => setRemember(e.target.checked)}
              />
              连接成功后保存该连接
            </label>
            <label className="checkbox">
              <input
                type="checkbox"
                checked={rememberPassword}
                disabled={!remember}
                onChange={(e) => setRememberPassword(e.target.checked)}
              />
              同时记住密码（存入系统凭据管理器）
            </label>
            <button className="primary" disabled={!!busy} onClick={testConnection}>
              测试连接
            </button>
          </div>

          <div className="card">
            <div className="card-head">
              <h2>数据库</h2>
            </div>
            <select
              value={database}
              onChange={(e) => setDatabase(e.target.value)}
              disabled={databases.length === 0}
            >
              {databases.length === 0 && <option value="">请先测试连接</option>}
              {databases.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
            <button disabled={!database || !!busy} onClick={loadTables}>
              读取表结构
            </button>
          </div>

          <div className="card">
            <div className="card-head">
              <h2>概览</h2>
            </div>
            <div className="summary">
              <div>
                <span>已选 / 总数</span>
                <strong>
                  {selectedCount} / {tables.length} 张表
                </strong>
              </div>
              <div>
                <span>列总数</span>
                <strong>{totalColumns} 列</strong>
              </div>
              <div>
                <span>输出</span>
                <strong>
                  {formatMeta.label}
                  {format === "excel" ? (sheetMode === "multi" ? " · 每表一页" : " · 单页") : ""}
                </strong>
              </div>
            </div>
          </div>
        </section>

        {/* 右栏：表列表 + 底部导出栏 */}
        <section className="panel right">
          <div className="toolbar">
            <div>
              <strong>{database || "未选择数据库"}</strong>
              <span className="muted">
                {" "}
                {tables.length > 0 &&
                  `· 共 ${tables.length} 张表 / ${totalColumns} 列 · 已选 ${selectedCount}`}
              </span>
            </div>
            <input
              ref={searchRef}
              className="search"
              placeholder="搜索表名或注释… (Ctrl+F)"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              disabled={tables.length === 0}
            />
          </div>

          {tables.length > 0 && (
            <div className="bulk">
              <button onClick={selectAllVisible}>全选{filter ? "筛选结果" : ""}</button>
              <button onClick={invertVisible}>反选{filter ? "筛选结果" : ""}</button>
              <button onClick={clearAll}>全不选</button>
              <span className="muted">
                当前显示 {visibleTables.length} 张，已选 {selectedCount} 张
              </span>
            </div>
          )}

          {busy && (
            <div className="toast info">
              <div className="row">
                <span className="spinner" />
                <span>{busy}</span>
                {exportProgress && (
                  <span className="muted">
                    {exportProgress.done} / {exportProgress.total} 张表
                  </span>
                )}
              </div>
              {exportProgress && exportProgress.total > 0 && (
                <div className="progress-bar">
                  <div
                    className="progress-fill"
                    style={{
                      width: `${Math.min(100, Math.round((exportProgress.done / exportProgress.total) * 100))}%`,
                    }}
                  />
                </div>
              )}
            </div>
          )}
          {error && <div className="toast error">{error}</div>}

          {result && (
            <div className="toast success">
              <button className="close" title="关闭" onClick={() => setResult(null)}>
                ×
              </button>
              <div className="row between">
                <div>导出成功：{result.out_path}</div>
                <button className="small" onClick={() => revealPath(result.out_path)}>
                  打开所在文件夹
                </button>
              </div>
              <div className="muted">
                表数量 {result.table_count}
                {result.sheet_count > 0 && ` · 工作表 ${result.sheet_count}`}
                {" · "}耗时 {result.elapsed_ms} ms
              </div>
              {result.renamed.length > 0 && (
                <details>
                  <summary>
                    有 {result.renamed.length} 个工作表因名称超长/非法/重复被自动改名
                  </summary>
                  <ul>
                    {result.renamed.slice(0, 50).map((r) => (
                      <li key={r.original}>
                        {r.original} → {r.actual}
                      </li>
                    ))}
                  </ul>
                  {result.renamed.length > 50 && (
                    <p className="muted">仅显示前 50 条，其余 {result.renamed.length - 50} 条略</p>
                  )}
                </details>
              )}
            </div>
          )}

          <div className="table-wrap">
            {visibleTables.length === 0 ? (
              <div className="empty">
                {tables.length === 0 ? "选择数据库后点击「读取表结构」" : "没有匹配的表"}
              </div>
            ) : (
              <table>
                <thead>
                  <tr>
                    <th style={{ width: 36 }} />
                    <th style={{ width: 52 }}>#</th>
                    <th className="sortable" onClick={() => toggleSort("name")}>
                      表名{sortMark("name")}
                    </th>
                    <th className="sortable" onClick={() => toggleSort("comment")}>
                      注释{sortMark("comment")}
                    </th>
                    <th
                      className="sortable"
                      style={{ width: 72 }}
                      onClick={() => toggleSort("columns")}
                    >
                      列数{sortMark("columns")}
                    </th>
                    <th style={{ width: 56 }} />
                  </tr>
                </thead>
                <tbody>
                  {visibleTables.map((t, i) => (
                    <tr
                      key={t.name}
                      className={selected.has(t.name) ? "selected" : ""}
                      onClick={() => toggleTable(t.name)}
                      onDoubleClick={() => openPreview(t)}
                    >
                      <td className="center">
                        <input
                          type="checkbox"
                          checked={selected.has(t.name)}
                          onChange={() => toggleTable(t.name)}
                          onClick={(e) => e.stopPropagation()}
                        />
                      </td>
                      <td className="num">{i + 1}</td>
                      <td className="mono">
                        <button
                          className="link"
                          title="查看表结构"
                          onClick={(e) => {
                            e.stopPropagation();
                            openPreview(t);
                          }}
                        >
                          {t.name}
                        </button>
                      </td>
                      <td>{t.comment}</td>
                      <td className="num">{t.column_count}</td>
                      <td className="row-actions">
                        <button
                          className="ghost"
                          title="查看表结构"
                          onClick={(e) => {
                            e.stopPropagation();
                            openPreview(t);
                          }}
                        >
                          结构
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>

          <div className="history">
            <div className="toolbar">
              <strong>最近导出</strong>
              {history.length > 0 && (
                <button className="small" onClick={clearHistory}>
                  清空
                </button>
              )}
            </div>
            {history.length === 0 ? (
              <div className="muted small">还没有导出记录</div>
            ) : (
              <ul>
                {history.map((h, idx) => (
                  <li key={`${h.time}-${idx}`} onClick={() => revealPath(h.out_path)}>
                    <span className="muted">{h.time}</span>
                    <span className="mono">{h.database}</span>
                    <span className="tag">{h.format}</span>
                    <span className="muted">{h.table_count} 表</span>
                    <span className="path">{h.out_path}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          {/* 底部固定导出栏 */}
          <div className="export-bar">
            <div className="field">
              <label>格式</label>
              <select value={format} onChange={(e) => setFormat(e.target.value)}>
                {FORMATS.map((f) => (
                  <option key={f.value} value={f.value}>
                    {f.label}
                  </option>
                ))}
              </select>
            </div>
            <div className="field">
              <label>Sheet 模式</label>
              <select
                value={sheetMode}
                onChange={(e) => setSheetMode(e.target.value)}
                disabled={format !== "excel"}
              >
                <option value="multi">每表一个 Sheet</option>
                <option value="single">单个 Sheet</option>
              </select>
            </div>
            <div className="field grow">
              <label>标题</label>
              <input value={title} onChange={(e) => setTitle(e.target.value)} />
            </div>
            <div className="field grow">
              <label>导出目录</label>
              <div className="row">
                <input value={outDir} onChange={(e) => setOutDir(e.target.value)} />
                <button onClick={chooseDirectory} title="选择目录">
                  浏览…
                </button>
                <button onClick={() => revealPath(outDir)} title="打开目录">
                  打开
                </button>
              </div>
            </div>
            <button className="primary export-btn" disabled={!database || !!busy} onClick={doExport}>
              导出{selectedCount > 0 ? ` ${selectedCount} 张表` : ""}
            </button>
          </div>
        </section>
      </main>

      <ConnectionManager
        open={managerOpen}
        connections={connections}
        activeId={activeId}
        onClose={() => setManagerOpen(false)}
        onRefresh={refreshState}
        onUse={applyConnection}
      />

      <TableDetail
        open={!!previewTable}
        table={previewTable}
        columns={previewColumns}
        loading={previewLoading}
        error={previewError}
        onClose={() => setPreviewTable(null)}
      />
    </div>
  );
}

export default App;
