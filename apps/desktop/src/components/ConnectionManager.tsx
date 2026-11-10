import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SavedConnection } from "../types";

interface Props {
  open: boolean;
  connections: SavedConnection[];
  activeId: string | null;
  onClose: () => void;
  /** 连接列表发生变化（新增/删除/切换）后刷新 */
  onRefresh: () => Promise<unknown>;
  /** 选中某个连接作为当前连接 */
  onUse: (conn: SavedConnection) => void;
}

interface Draft {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
}

const EMPTY: Draft = { id: "", name: "", host: "127.0.0.1", port: 3306, user: "root" };

function errText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

export function ConnectionManager({ open, connections, activeId, onClose, onRefresh, onUse }: Props) {
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  // 关闭弹窗时重置编辑状态
  useEffect(() => {
    if (!open) {
      setDraft(null);
      setError("");
    }
  }, [open]);

  if (!open) return null;

  const startAdd = () => {
    setError("");
    setDraft({ ...EMPTY });
  };

  const startEdit = (conn: SavedConnection) => {
    setError("");
    setDraft({ ...conn });
  };

  const submit = async () => {
    if (!draft) return;
    const name = draft.name.trim() || `${draft.user}@${draft.host}`;
    if (!draft.host.trim()) {
      setError("主机不能为空");
      return;
    }
    setBusy(true);
    setError("");
    try {
      await invoke<SavedConnection>("save_connection", {
        conn: {
          id: draft.id,
          name,
          host: draft.host.trim(),
          port: draft.port,
          user: draft.user.trim(),
        },
      });
      await onRefresh();
      setDraft(null);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (conn: SavedConnection) => {
    if (!confirm(`确定删除连接「${conn.name}」？`)) return;
    setBusy(true);
    setError("");
    try {
      await invoke("delete_connection", { id: conn.id });
      await onRefresh();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h3>连接管理</h3>
          <button className="small" onClick={onClose}>
            关闭
          </button>
        </div>

        {error && <div className="toast error">{error}</div>}

        {connections.length === 0 ? (
          <div className="muted">还没有保存的连接，点击下方「新增连接」。</div>
        ) : (
          <table className="conn-table">
            <thead>
              <tr>
                <th>名称</th>
                <th>地址</th>
                <th>用户</th>
                <th style={{ width: 150 }}>操作</th>
              </tr>
            </thead>
            <tbody>
              {connections.map((c) => (
                <tr key={c.id} className={c.id === activeId ? "active" : ""}>
                  <td>
                    {c.name}
                    {c.id === activeId && <span className="tag">当前</span>}
                  </td>
                  <td className="mono">
                    {c.host}:{c.port}
                  </td>
                  <td className="mono">{c.user}</td>
                  <td>
                    <div className="row">
                      <button
                        className="small"
                        disabled={busy || c.id === activeId}
                        onClick={() => {
                          onUse(c);
                          onClose();
                        }}
                      >
                        使用
                      </button>
                      <button className="small" disabled={busy} onClick={() => startEdit(c)}>
                        编辑
                      </button>
                      <button className="small danger" disabled={busy} onClick={() => remove(c)}>
                        删除
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        {draft ? (
          <div className="conn-form">
            <h4>{draft.id ? "编辑连接" : "新增连接"}</h4>
            <div className="field">
              <label>名称</label>
              <input
                value={draft.name}
                placeholder={`${draft.user}@${draft.host}`}
                onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              />
            </div>
            <div className="row">
              <div className="field grow">
                <label>主机</label>
                <input
                  value={draft.host}
                  onChange={(e) => setDraft({ ...draft, host: e.target.value })}
                />
              </div>
              <div className="field" style={{ width: 110 }}>
                <label>端口</label>
                <input
                  type="number"
                  value={draft.port}
                  onChange={(e) => setDraft({ ...draft, port: Number(e.target.value) || 3306 })}
                />
              </div>
            </div>
            <div className="field">
              <label>用户</label>
              <input
                value={draft.user}
                onChange={(e) => setDraft({ ...draft, user: e.target.value })}
              />
            </div>
            <div className="row">
              <button className="primary" disabled={busy} onClick={submit}>
                保存
              </button>
              <button disabled={busy} onClick={() => setDraft(null)}>
                取消
              </button>
              <span className="muted small">密码不会保存，每次连接时输入</span>
            </div>
          </div>
        ) : (
          <button onClick={startAdd}>+ 新增连接</button>
        )}
      </div>
    </div>
  );
}
