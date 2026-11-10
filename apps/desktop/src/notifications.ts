/** 通知中心：瞬时 Toast + 可追溯的消息栈（参考 IDEA 的 Notifications 模型）。 */

export type NoticeKind = "success" | "error" | "info";

export interface NoticeAction {
  label: string;
  onClick: () => void;
}

export interface Notice {
  id: string;
  kind: NoticeKind;
  title: string;
  detail?: string;
  action?: NoticeAction;
  time: Date;
  read: boolean;
}

export const NOTICE_ICON: Record<NoticeKind, string> = {
  success: "✓",
  error: "!",
  info: "i",
};

let seq = 0;
export function createNotice(
  kind: NoticeKind,
  title: string,
  detail?: string,
  action?: NoticeAction,
): Notice {
  seq += 1;
  return {
    id: `n_${Date.now()}_${seq}`,
    kind,
    title,
    detail,
    action,
    time: new Date(),
    read: false,
  };
}

export function formatTime(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}
