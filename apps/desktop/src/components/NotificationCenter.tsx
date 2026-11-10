import { useEffect, useRef, useState } from "react";
import { formatTime, NOTICE_ICON, type Notice } from "../notifications";

interface Props {
  notices: Notice[];
  unread: number;
  onOpen: () => void;
  onClear: () => void;
  onRemove: (id: string) => void;
}

/** 通知中心：铃铛入口 + 消息栈面板。 */
export function NotificationCenter({ notices, unread, onOpen, onClear, onRemove }: Props) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  // 点击外部关闭
  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  // Esc 关闭
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next) onOpen();
  };

  return (
    <div className="notice-center" ref={wrapRef}>
      <button
        className={`bell ${open ? "active" : ""}`}
        onClick={toggle}
        title="通知"
        aria-label={`通知${unread > 0 ? `（${unread} 条未读）` : ""}`}
      >
        <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
          <path
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
            d="M18 8a6 6 0 1 0-12 0c0 7-3 8-3 8h18s-3-1-3-8"
          />
          <path
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            d="M13.7 21a2 2 0 0 1-3.4 0"
          />
        </svg>
        {unread > 0 && <span className="badge">{unread > 99 ? "99+" : unread}</span>}
      </button>

      {open && (
        <div className="notice-panel">
          <div className="notice-panel-head">
            <strong>通知</strong>
            <div className="row">
              {notices.length > 0 && (
                <button className="small" onClick={onClear}>
                  全部清空
                </button>
              )}
              <button className="small" onClick={() => setOpen(false)}>
                关闭
              </button>
            </div>
          </div>

          {notices.length === 0 ? (
            <div className="notice-empty">还没有通知</div>
          ) : (
            <ul className="notice-list">
              {notices.map((n) => (
                <li key={n.id} className={`notice-item ${n.kind} ${n.read ? "" : "unread"}`}>
                  <span className={`notice-icon ${n.kind}`}>{NOTICE_ICON[n.kind]}</span>
                  <div className="notice-body">
                    <div className="notice-title">
                      <span>{n.title}</span>
                      <span className="muted small">{formatTime(n.time)}</span>
                    </div>
                    {n.detail && <pre className="notice-detail">{n.detail}</pre>}
                    {n.action && (
                      <button
                        className="small"
                        onClick={() => {
                          n.action?.onClick();
                        }}
                      >
                        {n.action.label}
                      </button>
                    )}
                  </div>
                  <button
                    className="notice-remove"
                    title="删除这条通知"
                    onClick={() => onRemove(n.id)}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
