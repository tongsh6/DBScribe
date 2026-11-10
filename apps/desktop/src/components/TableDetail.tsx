import type { ColumnView, TableSummary } from "../types";

interface Props {
  open: boolean;
  table: TableSummary | null;
  columns: ColumnView[] | null;
  loading: boolean;
  error: string;
  onClose: () => void;
}

export function TableDetail({ open, table, columns, loading, error, onClose }: Props) {
  if (!open || !table) return null;

  return (
    <div className="drawer-backdrop" onClick={onClose}>
      <aside className="drawer" onClick={(e) => e.stopPropagation()}>
        <div className="drawer-head">
          <div>
            <h3 className="mono">{table.name}</h3>
            <p className="muted">{table.comment || "（无注释）"}</p>
          </div>
          <button className="small" onClick={onClose}>
            关闭
          </button>
        </div>

        <div className="drawer-meta muted">
          {table.column_count} 个列
          {columns && columns.length > 0 &&
            ` · 主键 ${columns.filter((c) => c.is_primary_key).length} 个`}
        </div>

        {loading && <div className="toast info">正在读取列结构…</div>}
        {error && <div className="toast error">{error}</div>}

        {columns && columns.length > 0 && (
          <div className="drawer-table">
            <table>
              <thead>
                <tr>
                  <th style={{ width: "4%" }}>#</th>
                  <th style={{ width: "22%" }}>列名</th>
                  <th style={{ width: "12%" }}>类型</th>
                  <th style={{ width: "7%" }}>长度</th>
                  <th style={{ width: "5%" }}>主键</th>
                  <th style={{ width: "5%" }}>自增</th>
                  <th style={{ width: "5%" }}>可空</th>
                  <th style={{ width: "40%" }}>注释</th>
                </tr>
              </thead>
              <tbody>
                {columns.map((c) => (
                  <tr key={c.name}>
                    <td className="num">{c.order}</td>
                    <td className="mono">{c.name}</td>
                    <td className="mono">{c.data_type}</td>
                    <td className="num">{c.length || "-"}</td>
                    <td className="center">{c.is_primary_key ? "√" : ""}</td>
                    <td className="center">{c.is_auto_increment ? "√" : ""}</td>
                    <td className="center">{c.nullable ? "√" : ""}</td>
                    <td>{c.comment}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </aside>
    </div>
  );
}
