import { NOTICE_ICON, type Notice } from "../notifications";

interface Props {
  toasts: Notice[];
  onDismiss: (id: string) => void;
}

/** 右下角瞬时提示：不占用主布局，几秒后自动消失。 */
export function ToastStack({ toasts, onDismiss }: Props) {
  if (toasts.length === 0) return null;

  return (
    <div className="toast-stack">
      {toasts.map((t) => (
        <div key={t.id} className={`toast-item ${t.kind}`} onClick={() => onDismiss(t.id)}>
          <span className={`notice-icon ${t.kind}`}>{NOTICE_ICON[t.kind]}</span>
          <div className="toast-text">
            <div className="toast-title">{t.title}</div>
            {t.detail && <div className="toast-detail">{t.detail.split("\n")[0]}</div>}
          </div>
        </div>
      ))}
    </div>
  );
}
