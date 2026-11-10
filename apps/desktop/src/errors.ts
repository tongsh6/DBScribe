/** 把底层错误翻译成「发生了什么 + 该怎么办」，避免直接把 MySQL / 系统原文甩给用户。 */

export interface ExplainedError {
  title: string;
  detail: string;
}

/** 在原文后追加建议，保留原始信息便于排查。 */
function withAdvice(raw: string, advice: string): string {
  return `${raw.trim()}\n\n建议：${advice}`;
}

export function explainError(err: unknown): ExplainedError {
  const raw = typeof err === "string" ? err : err instanceof Error ? err.message : String(err);
  const m = raw.toLowerCase();

  if (m.includes("access denied")) {
    return {
      title: "账号或密码不正确",
      detail: withAdvice(
        raw,
        "检查用户名与密码；确认该账号允许从当前主机连接（MySQL 的 user@host 授权，localhost 与 127.0.0.1 可能是不同条目）。",
      ),
    };
  }

  if (
    m.includes("connection refused") ||
    m.includes("can't connect") ||
    m.includes("cannot connect") ||
    m.includes("failed to connect")
  ) {
    return {
      title: "无法连接到数据库",
      detail: withAdvice(
        raw,
        "确认 MySQL/MariaDB 服务已启动；检查主机与端口是否正确；若数据库运行在 Docker/虚拟机中，确认端口已映射到本机。",
      ),
    };
  }

  if (m.includes("unknown database")) {
    return {
      title: "数据库不存在",
      detail: withAdvice(raw, "检查数据库名拼写；也可以先点「测试连接」，下拉里会列出所有可访问的库。"),
    };
  }

  if (m.includes("unknown host") || m.includes("no such host")) {
    return {
      title: "主机地址无法解析",
      detail: withAdvice(raw, "检查主机名是否拼写正确；如果是远程库，确认网络可达。"),
    };
  }

  if (m.includes("timed out") || m.includes("timeout")) {
    return {
      title: "连接超时",
      detail: withAdvice(raw, "检查网络连通性与防火墙设置；远程数据库可能限制了来源 IP。"),
    };
  }

  if (m.includes("permission denied") || m.includes("access is denied") || m.includes("拒绝访问")) {
    return {
      title: "没有文件写入权限",
      detail: withAdvice(
        raw,
        "换一个有写权限的导出目录（比如用户目录下的文件夹）；若目标文件正被 Excel 打开，请先关闭再重试。",
      ),
    };
  }

  if (m.includes("os error 32") || m.includes("being used by another process")) {
    return {
      title: "文件被占用",
      detail: withAdvice(raw, "目标文件可能正在 Excel 中打开，关闭后重试；也可以换一个文档名称。"),
    };
  }

  if (m.includes("no such file") || m.includes("not found")) {
    return {
      title: "路径不存在",
      detail: withAdvice(raw, "检查导出目录是否存在，或改用「浏览…」重新选择。"),
    };
  }

  if (m.includes("no tables") || m.includes("没有任何基础表")) {
    return {
      title: "这个库里没有表",
      detail: withAdvice(raw, "确认选中的是目标数据库；空库或只有视图的库不会有可导出的表。"),
    };
  }

  return { title: "操作失败", detail: raw };
}
