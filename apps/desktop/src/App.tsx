import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

interface ConnectionParams {
  host: string;
  port: number;
  user: string;
  password: string;
}

interface TableSummary {
  name: string;
  comment: string;
  column_count: number;
}

interface RenamedSheet {
  original: string;
  actual: string;
}

interface ExportResult {
  out_path: string;
  table_count: number;
  sheet_count: number;
  renamed: RenamedSheet[];
  elapsed_ms: number;
}

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
  const [conn, setConn] = useState<ConnectionParams>({
    host: "127.0.0.1",
    port: 3306,
    user: "root",
    password: "",
  });
  const [databases, setDatabases] = useState<string[]>([]);
  const [database, setDatabase] = useState("");
  const [tables, setTables] = useState<TableSummary[]>([]);
  const [filter, setFilter] = useState("");
  const [format, setFormat] = useState("excel");
  const [sheetMode, setSheetMode] = useState("multi");
  const [title, setTitle] = useState("");
  const [outDir, setOutDir] = useState(".");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [result, setResult] = useState<ExportResult | null>(null);

  const formatMeta = FORMATS.find((f) => f.value === format) ?? FORMATS[0];

  // 切库时自动带出默认标题
  useEffect(() => {
    if (database) setTitle(`${database}数据库设计文档`);
  }, [database]);

  const testConnection = useCallback(async () => {
    setBusy("正在连接数据库…");
    setError("");
    setResult(null);
    try {
      const msg = await invoke<string>("test_connection", { conn });
      const list = await invoke<string[]>("list_databases_cmd", { conn });
      setDatabases(list);
      setDatabase((prev) => prev || list[0] || "");
      setError("");
      setResult(null);
      alert(msg);
    } catch (e) {
      setError(errText(e));
      setDatabases([]);
    } finally {
      setBusy(null);
    }
  }, [conn]);

  const loadTables = useCallback(async () => {
    if (!database) return;
    setBusy("正在读取表结构…");
    setError("");
    setResult(null);
    try {
      const list = await invoke<TableSummary[]>("list_tables_cmd", {
        conn,
        database,
      });
      setTables(list);
    } catch (e) {
      setTables([]);
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  }, [conn, database]);

  const doExport = useCallback(async () => {
    if (!database) {
      setError("请先选择数据库");
      return;
    }
    const fileName = `${(title || `${database}数据库设计文档`).replace(/[\\/:*?"<>|]/g, "_")}.${formatMeta.ext}`;
    const sep = outDir.includes("\\") ? "\\" : "/";
    const outPath = outDir.endsWith("/") || outDir.endsWith("\\") ? `${outDir}${fileName}` : `${outDir}${sep}${fileName}`;

    setBusy("正在导出文档…");
    setError("");
    setResult(null);
    try {
      const res = await invoke<ExportResult>("export_document", {
        conn,
        params: {
          database,
          title,
          format,
          sheet_mode: sheetMode,
          out_path: outPath,
        },
      });
      setResult(res);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  }, [conn, database, title, format, sheetMode, outDir, formatMeta.ext]);

  const visibleTables = useMemo(() => {
    const kw = filter.trim().toLowerCase();
    if (!kw) return tables;
    return tables.filter(
      (t) =>
        t.name.toLowerCase().includes(kw) ||
        t.comment.toLowerCase().includes(kw),
    );
  }, [tables, filter]);

  const totalColumns = useMemo(
    () => tables.reduce((sum, t) => sum + t.column_count, 0),
    [tables],
  );

  return (
    <div className="app">
      <header>
        <div className="brand">
          <span className="logo">DB</span>
          <div>
            <h1>DBScribe</h1>
            <p>跨平台数据库结构文档工具</p>
          </div>
        </div>
      </header>

      <main>
        {/* 左栏：连接与导出设置 */}
        <section className="panel left">
          <h2>连接</h2>
          <div className="field">
            <label>主机</label>
            <input
              value={conn.host}
              onChange={(e) => setConn({ ...conn, host: e.target.value })}
            />
          </div>
          <div className="field">
            <label>端口</label>
            <input
              type="number"
              value={conn.port}
              onChange={(e) =>
                setConn({ ...conn, port: Number(e.target.value) || 3306 })
              }
            />
          </div>
          <div className="field">
            <label>用户</label>
            <input
              value={conn.user}
              onChange={(e) => setConn({ ...conn, user: e.target.value })}
            />
          </div>
          <div className="field">
            <label>密码</label>
            <input
              type="password"
              value={conn.password}
              onChange={(e) => setConn({ ...conn, password: e.target.value })}
            />
          </div>
          <button className="primary" disabled={!!busy} onClick={testConnection}>
            测试连接
          </button>

          <h2>数据库</h2>
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

          <h2>导出设置</h2>
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
              <option value="multi">每个表一个 Sheet</option>
              <option value="single">所有表放一个 Sheet</option>
            </select>
          </div>
          <div className="field">
            <label>标题</label>
            <input value={title} onChange={(e) => setTitle(e.target.value)} />
          </div>
          <div className="field">
            <label>输出目录</label>
            <input value={outDir} onChange={(e) => setOutDir(e.target.value)} />
          </div>
          <button
            className="primary"
            disabled={!database || !!busy}
            onClick={doExport}
          >
            开始导出
          </button>
        </section>

        {/* 右栏：表列表与结果 */}
        <section className="panel right">
          <div className="toolbar">
            <div>
              <strong>{database || "未选择数据库"}</strong>
              <span className="muted">
                {" "}
                {tables.length > 0 &&
                  `· ${tables.length} 张表 · ${totalColumns} 个列`}
              </span>
            </div>
            <input
              className="search"
              placeholder="搜索表名或注释…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              disabled={tables.length === 0}
            />
          </div>

          {busy && <div className="toast info">{busy}</div>}
          {error && <div className="toast error">{error}</div>}

          {result && (
            <div className="toast success">
              <div>
                导出成功：{result.out_path}
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
                    <p className="muted">
                      仅显示前 50 条，其余 {result.renamed.length - 50} 条略
                    </p>
                  )}
                </details>
              )}
            </div>
          )}

          <div className="table-wrap">
            {visibleTables.length === 0 ? (
              <div className="empty">
                {tables.length === 0
                  ? "选择数据库后点击「读取表结构」"
                  : "没有匹配的表"}
              </div>
            ) : (
              <table>
                <thead>
                  <tr>
                    <th style={{ width: 56 }}>#</th>
                    <th>表名</th>
                    <th>注释</th>
                    <th style={{ width: 72 }}>列数</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleTables.map((t, i) => (
                    <tr key={t.name}>
                      <td className="num">{i + 1}</td>
                      <td className="mono">{t.name}</td>
                      <td>{t.comment}</td>
                      <td className="num">{t.column_count}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        </section>
      </main>
    </div>
  );
}

export default App;
