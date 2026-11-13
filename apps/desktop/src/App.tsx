import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { ConnectionManager } from "./components/ConnectionManager";
import { EmptyGuide } from "./components/EmptyGuide";
import { NotificationCenter } from "./components/NotificationCenter";
import { TableSkeleton } from "./components/TableSkeleton";
import { ToastStack } from "./components/ToastStack";
import { TableDetail } from "./components/TableDetail";
import { createNotice, type Notice, type NoticeKind } from "./notifications";
import { explainError } from "./errors";
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

const DB_TYPES = [
  { value: "mysql", label: "MySQL / MariaDB", port: 3306 },
  { value: "postgres", label: "PostgreSQL", port: 5432 },
  { value: "sqlite", label: "SQLite（本地文件）", port: 0 },
  { value: "sqlserver", label: "SQL Server", port: 1433 },
  { value: "oracle", label: "Oracle", port: 1521 },
  { value: "oceanbase", label: "OceanBase（MySQL 兼容）", port: 2881 },
  { value: "dameng", label: "达梦 DM8（需 ODBC 驱动）", port: 5236 },
];

const FORMATS = [
  { value: "excel", label: "Excel (.xlsx)", ext: "xlsx" },
  { value: "markdown", label: "Markdown (.md)", ext: "md" },
  { value: "html", label: "HTML (.html)", ext: "html" },
  { value: "json", label: "JSON (.json)", ext: "json" },
  { value: "xml", label: "XML (.xml)", ext: "xml" },
  { value: "word", label: "Word (.docx)", ext: "docx" },
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
    db_type: "mysql",
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
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const [sortKey, setSortKey] = useState<"name" | "comment" | "columns">("name");
  const [sortAsc, setSortAsc] = useState(true);
  const [format, setFormat] = useState("excel");
  const [sheetMode, setSheetMode] = useState("multi");
  const [title, setTitle] = useState("");
  const [outDir, setOutDir] = useState(".");
  const [busy, setBusy] = useState<"connect" | "load" | "export" | null>(null);
  const [exportProgress, setExportProgress] = useState<{ done: number; total: number } | null>(null);
  const [damengDriver, setDamengDriver] = useState<{
    available: boolean;
    description: string;
    guide: string;
  } | null>(null);
  const [oracleProgress, setOracleProgress] = useState<{
    done: number;
    total: number;
    percent: number;
  } | null>(null);
  const [notices, setNotices] = useState<Notice[]>([]);
  const [toasts, setToasts] = useState<Notice[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [activeRow, setActiveRow] = useState(-1);
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
  const unread = useMemo(() => notices.filter((n) => !n.read).length, [notices]);

  /** 推送一条通知：右下角浮出（自动消失）+ 进入通知中心 */
  const notify = useCallback(
    (kind: NoticeKind, title: string, detail?: string, action?: Notice["action"]) => {
      const notice = createNotice(kind, title, detail, action);
      setNotices((prev) => [notice, ...prev].slice(0, 50));
      setToasts((prev) => [...prev, notice].slice(-3));
      setTimeout(() => {
        setToasts((prev) => prev.filter((t) => t.id !== notice.id));
      }, 4200);
    },
    [],
  );

  // 搜索防抖：避免大库每次按键都全量重排
  useEffect(() => {
    const t = setTimeout(() => setDebouncedFilter(filter), 150);
    return () => clearTimeout(t);
  }, [filter]);

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
    setConn({ db_type: c.db_type || "mysql", host: c.host, port: c.port, user: c.user, password: "" });
    setDatabases([]);
    setDatabase("");
    setTables([]);
    setSelected(new Set());

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

  // 已保存密码时，启动后自动连接并载入第一个库（静默失败，不打扰用户）
  const autoConnected = useRef(false);
  useEffect(() => {
    if (autoConnected.current || !prefsReady) return;
    const active = connections.find((c) => c.id === activeId);
    if (!active?.has_password || !conn.password || databases.length > 0) return;

    autoConnected.current = true;
    void (async () => {
      try {
        const list = await invoke<string[]>("list_databases_cmd", { conn });
        setDatabases(list);
        const first = list[0];
        if (first) {
          setDatabase(first);
          const rows = await invoke<TableSummary[]>("list_tables_cmd", {
            conn,
            database: first,
          });
          setTables(rows);
          setSelected(new Set(rows.map((t) => t.name)));
        }
      } catch {
        /* 静默失败：用户手动点「测试连接」可看到具体原因 */
      }
    })();
  }, [prefsReady, connections, activeId, conn, databases.length]);

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
    if (database) setTitle(`${databaseDisplay}数据库设计文档`);
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
          db_type: conn.db_type,
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
    setBusy("connect");
    try {
      // Oracle 首次使用需要先安装客户端（懒安装：只有选 Oracle 才下载）
      if (conn.db_type === "oracle") {
        const installed = await invoke<boolean>("oracle_client_status");
        if (!installed) {
          notify(
            "info",
            "首次使用 Oracle 需要下载客户端",
            "约 60MB，来自 Oracle 官方下载源；下载完成后会自动继续连接。",
          );
          await invoke<string>("ensure_oracle_client");
          setOracleProgress(null);
          notify("success", "Oracle 客户端已就绪");
        }
      }
      // Oracle 通过服务名建立连接
      const dbArg = conn.db_type === "oracle" ? database || "XE" : undefined;
      const msg = await invoke<string>("test_connection", { conn, database: dbArg });
      const list = await invoke<string[]>("list_databases_cmd", { conn, database: dbArg });
      setDatabases(list);
      setDatabase((prev) => (prev && list.includes(prev) ? prev : list[0] || ""));
      await rememberConnection();
      notify("success", msg);
    } catch (e) {
      setDatabases([]);
      const explained = explainError(e);
      notify("error", explained.title, explained.detail);
    } finally {
      setBusy(null);
    }
  }, [conn, rememberConnection, database, notify]);

  const loadTables = useCallback(
    async (db?: string) => {
      const target = db ?? database;
      if (!target) return;
      setBusy("load");
      try {
        const list = await invoke<TableSummary[]>("list_tables_cmd", {
          conn,
          database: target,
        });
        setTables(list);
        setSelected(new Set(list.map((t) => t.name)));
        await rememberConnection();
      } catch (e) {
        setTables([]);
        setSelected(new Set());
        const explained = explainError(e);
        notify("error", explained.title, explained.detail);
      } finally {
        setBusy(null);
      }
    },
    [conn, database, rememberConnection, notify],
  );

  /** 切换数据库后自动读取表结构（省掉一次点击） */
  const handleSelectDatabase = useCallback(
    (db: string) => {
      setDatabase(db);
      setTables([]);
      setSelected(new Set());
      if (db) void loadTables(db);
    },
    [loadTables],
  );

  const isSqlite = conn.db_type === "sqlite";

  /** 用于标题 / 文件名的库显示名：SQLite 取文件名（去掉扩展名） */
  const databaseDisplay = useMemo(() => {
    if (conn.db_type === "sqlite" && database) {
      const base = database.split(/[\\/]/).pop() ?? database;
      return base.replace(/\.(db|sqlite|sqlite3)$/i, "");
    }
    return database;
  }, [conn.db_type, database]);

  /** SQLite：选中一个 .db 文件即视为连接，直接读取表结构 */
  const chooseDatabaseFile = useCallback(async () => {
    try {
      const file = await openDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "SQLite 数据库", extensions: ["db", "sqlite", "sqlite3"] }],
      });
      if (typeof file === "string" && file) {
        setDatabase(file);
        setDatabases([file]);
        await loadTables(file);
      }
    } catch (e) {
      const explained = explainError(e);
      notify("error", explained.title, explained.detail);
    }
  }, [loadTables, notify]);

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

  // 监听 Oracle 客户端下载进度
  useEffect(() => {
    const unlisten = listen<{ done: number; total: number; percent: number }>(
      "oracle-client-progress",
      (e) => setOracleProgress(e.payload),
    );
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // 切到达梦时检测 ODBC 驱动，未安装则展示安装引导
  useEffect(() => {
    if (conn.db_type !== "dameng") {
      setDamengDriver(null);
      return;
    }
    invoke<{ available: boolean; description: string; guide: string }>("dameng_driver_status")
      .then(setDamengDriver)
      .catch(() => setDamengDriver(null));
  }, [conn.db_type]);

  const visibleTables = useMemo(() => {
    // 搜索支持多个关键词（空格分隔），需全部命中
    const terms = debouncedFilter.trim().toLowerCase().split(/\s+/).filter(Boolean);
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
  }, [tables, debouncedFilter, sortKey, sortAsc]);

  // 键盘导航：上下键移动、空格勾选、Enter 预览（可访问性）
  const onTableKeyDown = (e: React.KeyboardEvent) => {
    if (visibleTables.length === 0) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setActiveRow((prev) => {
        const next =
          e.key === "ArrowDown"
            ? Math.min(prev + 1, visibleTables.length - 1)
            : Math.max(prev - 1, 0);
        return next;
      });
    } else if (e.key === " " && activeRow >= 0) {
      e.preventDefault();
      toggleTable(visibleTables[activeRow].name);
    } else if (e.key === "Enter" && activeRow >= 0) {
      e.preventDefault();
      openPreview(visibleTables[activeRow]);
    }
  };

  useEffect(() => {
    if (activeRow < 0) return;
    document.getElementById(`tbl-row-${activeRow}`)?.scrollIntoView({ block: "nearest" });
  }, [activeRow]);

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
      {
        const explained = explainError(e);
        notify("error", explained.title, explained.detail);
      }
    }
  }, [outDir]);

  const doExport = useCallback(async () => {
    if (!database) {
      notify("error", "请先选择数据库", "在左侧完成连接、选择数据库并点击「读取表结构」。");
      return;
    }
    if (selectedCount === 0) {
      notify("error", "请至少勾选一张表", "在表列表中勾选要导出的表，或用上方的「全选」。");
      return;
    }
    const fileName = `${(title || `${databaseDisplay}数据库设计文档`).replace(/[\\/:*?"<>|]/g, "_")}.${formatMeta.ext}`;
    const sep = outDir.includes("\\") ? "\\" : "/";
    const outPath =
      outDir.endsWith("/") || outDir.endsWith("\\")
        ? `${outDir}${fileName}`
        : `${outDir}${sep}${fileName}`;

    const chosen = tables.filter((t) => selected.has(t.name)).map((t) => t.name);
    const tablesParam = chosen.length === tables.length ? null : chosen;

    setBusy("export");
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

      // 导出结果走通知中心：Toast 一闪而过，详情进消息栈可追溯
      const lines = [
        res.out_path,
        `表数量 ${res.table_count}` +
          (res.sheet_count > 0 ? ` · 工作表 ${res.sheet_count}` : "") +
          ` · 耗时 ${res.elapsed_ms} ms`,
      ];
      if (res.renamed.length > 0) {
        lines.push(`\n有 ${res.renamed.length} 个工作表因名称超长/非法/重复被自动改名：`);
        for (const r of res.renamed.slice(0, 8)) lines.push(`  ${r.original} → ${r.actual}`);
        if (res.renamed.length > 8) lines.push(`  …其余 ${res.renamed.length - 8} 条略`);
      }
      notify("success", "导出成功", lines.join("\n"), {
        label: "打开文件夹",
        onClick: () => revealPath(res.out_path),
      });

      refreshState()
        .then((s) => setHistory(s.history ?? []))
        .catch(() => undefined);
    } catch (e) {
      {
        const explained = explainError(e);
        notify("error", explained.title, explained.detail);
      }
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
      {
        const explained = explainError(e);
        notify("error", explained.title, explained.detail);
      }
    }
  }, []);

  const clearHistory = useCallback(async () => {
    try {
      await invoke("clear_history");
      setHistory([]);
    } catch (e) {
      {
        const explained = explainError(e);
        notify("error", explained.title, explained.detail);
      }
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
        <div className="header-right">
          <div className="status">
            <span className={`dot ${connected ? "on" : ""}`} />
            <span className="muted">
              {connected
                ? `${activeConnection?.name ?? conn.user + "@" + conn.host}`
                : "未连接"}
            </span>
          </div>
          <NotificationCenter
            notices={notices}
            unread={unread}
            onOpen={() => setNotices((prev) => prev.map((n) => ({ ...n, read: true })))}
            onClear={() => setNotices([])}
            onRemove={(id) => setNotices((prev) => prev.filter((n) => n.id !== id))}
          />
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

            <div className="field">
              <label>数据库类型</label>
              <select
                value={conn.db_type}
                onChange={(e) => {
                  const next = DB_TYPES.find((d) => d.value === e.target.value);
                  setConn({
                    ...conn,
                    db_type: e.target.value,
                    port: next?.port ?? conn.port,
                  });
                  setDatabases([]);
                  setDatabase("");
                  setTables([]);
                  setSelected(new Set());
                }}
              >
                {DB_TYPES.map((d) => (
                  <option key={d.value} value={d.value}>
                    {d.label}
                  </option>
                ))}
              </select>
            </div>

            {conn.db_type === "dameng" && damengDriver && (
              <div className={`driver-tip ${damengDriver.available ? "ok" : "warn"}`}>
                {damengDriver.available ? (
                  <span>ODBC 驱动已就绪：{damengDriver.description}</span>
                ) : (
                  <>
                    <pre className="driver-guide">{damengDriver.guide}</pre>
                    <button
                      className="small"
                      onClick={() => {
                        invoke<{ available: boolean; description: string; guide: string }>(
                          "dameng_driver_status",
                        )
                          .then(setDamengDriver)
                          .catch(() => undefined);
                      }}
                    >
                      重新检测
                    </button>
                  </>
                )}
              </div>
            )}
            {conn.db_type === "oracle" && (
              <div className="field">
                <label>服务名（service name）</label>
                <input
                  value={database}
                  placeholder="XE"
                  onChange={(e) => setDatabase(e.target.value)}
                />
              </div>
            )}
            {isSqlite ? (
              <div className="field">
                <label>数据库文件</label>
                <div className="row">
                  <input
                    value={database}
                    placeholder="选择或输入 .db 文件路径"
                    onChange={(e) => setDatabase(e.target.value)}
                  />
                  <button onClick={chooseDatabaseFile}>浏览…</button>
                </div>
              </div>
            ) : (
              <>
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
                      onChange={(e) =>
                        setConn({ ...conn, port: Number(e.target.value) || conn.port })
                      }
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
              </>
            )}
            {!isSqlite && (
              <>
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
                <button
                  className="primary"
                  disabled={!!busy}
                  onClick={testConnection}
                  title={busy === "connect" ? "正在连接数据库…" : undefined}
                >
                  {busy === "connect" ? (
                    <>
                      <span className="spinner" />
                      {oracleProgress
                        ? `下载客户端 ${oracleProgress.percent}%`
                        : "连接中…"}
                    </>
                  ) : (
                    "测试连接"
                  )}
                </button>
              </>
            )}
          </div>

          <div className="card">
            <div className="card-head">
              <h2>数据库</h2>
            </div>
            <select
              value={database}
              onChange={(e) => handleSelectDatabase(e.target.value)}
              disabled={databases.length === 0}
            >
              {databases.length === 0 && <option value="">请先测试连接</option>}
              {databases.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
            <button
              disabled={!database || !!busy}
              onClick={() => void loadTables()}
              title={!database ? "请先在左侧选择数据库" : undefined}
            >
              {busy === "load" ? (
                <>
                  <span className="spinner" />
                  读取中…
                </>
              ) : (
                "读取表结构"
              )}
            </button>
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

          <div
            className="table-wrap"
            tabIndex={0}
            onKeyDown={onTableKeyDown}
            aria-label="表列表，可用上下键移动、空格勾选、回车查看结构"
          >
            {busy === "load" ? (
              <TableSkeleton />
            ) : visibleTables.length === 0 ? (
              <EmptyGuide connected={connected} database={database} tableCount={tables.length} />
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
                      id={`tbl-row-${i}`}
                      className={`${selected.has(t.name) ? "selected" : ""} ${i === activeRow ? "active" : ""}`}
                      onClick={() => {
                        setActiveRow(i);
                        toggleTable(t.name);
                      }}
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
              <button
                className="history-toggle"
                onClick={() => setHistoryOpen((v) => !v)}
                title={historyOpen ? "收起" : "展开最近导出"}
              >
                <span className="caret">{historyOpen ? "▾" : "▸"}</span>
                最近导出
                {history.length > 0 && <span className="muted">（{history.length}）</span>}
              </button>
              {historyOpen && history.length > 0 && (
                <button className="small" onClick={clearHistory}>
                  清空
                </button>
              )}
            </div>
            {historyOpen &&
              (history.length === 0 ? (
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
              ))}
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
            <div className="export-action">
              <button
                className="primary export-btn"
                disabled={!database || !!busy}
                onClick={doExport}
                title={
                  !database
                    ? "请先读取表结构"
                    : selectedCount === 0
                      ? "请至少勾选一张表"
                      : undefined
                }
              >
                {busy && exportProgress
                  ? `导出中 ${exportProgress.done}/${exportProgress.total}`
                  : `导出${selectedCount > 0 ? ` ${selectedCount} 张表` : ""}`}
              </button>
              {busy && exportProgress && exportProgress.total > 0 && (
                <div className="btn-progress">
                  <div
                    className="btn-progress-fill"
                    style={{
                      width: `${Math.min(100, Math.round((exportProgress.done / exportProgress.total) * 100))}%`,
                    }}
                  />
                </div>
              )}
            </div>
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

      <ToastStack
        toasts={toasts}
        onDismiss={(id) => setToasts((prev) => prev.filter((t) => t.id !== id))}
      />
    </div>
  );
}

export default App;
