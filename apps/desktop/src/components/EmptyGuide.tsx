interface Props {
  /** 是否已经连上数据库 */
  connected: boolean;
  /** 当前选中的库 */
  database: string;
  /** 是否已读取到表 */
  tableCount: number;
}

/**
 * 空状态引导：把「第一步该干什么」直接摆出来，而不是丢一句提示。
 * 步骤会随当前进度自动打勾 / 高亮。
 */
export function EmptyGuide({ connected, database, tableCount }: Props) {
  if (tableCount > 0) {
    return (
      <div className="empty">
        <div>
          <div>没有匹配的表</div>
          <div className="muted small" style={{ marginTop: 6 }}>
            试试清空搜索条件，或换一个关键词
          </div>
        </div>
      </div>
    );
  }

  const steps = [
    {
      done: connected,
      active: !connected,
      text: "在左侧填写连接信息，点击「测试连接」",
    },
    {
      done: connected && !!database,
      active: connected && !database,
      text: "选择要导出的数据库",
    },
    {
      done: false,
      active: connected && !!database,
      text: "点击「读取表结构」加载该库的表",
    },
    {
      done: false,
      active: false,
      text: "勾选表、选择格式与输出目录，点击「导出」",
    },
  ];

  return (
    <div className="empty-guide">
      <h3>{connected ? "下一步" : "开始使用"}</h3>
      <ol>
        {steps.map((s, i) => (
          <li key={i} className={s.done ? "done" : s.active ? "active" : ""}>
            <span className="step-no">{s.done ? "✓" : i + 1}</span>
            <span className="step-text">{s.text}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}
