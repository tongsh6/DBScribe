/** 表格骨架屏：读取表结构时的占位，避免出现大片空白。 */
export function TableSkeleton({ rows = 8 }: { rows?: number }) {
  return (
    <div className="skeleton" aria-busy="true" aria-label="正在加载表结构">
      <div className="skeleton-row skeleton-head">
        <span className="bar" style={{ width: "4%" }} />
        <span className="bar" style={{ width: "22%" }} />
        <span className="bar" style={{ width: "48%" }} />
        <span className="bar" style={{ width: "8%" }} />
      </div>
      {Array.from({ length: rows }).map((_, i) => (
        <div className="skeleton-row" key={i}>
          <span className="bar" style={{ width: "4%" }} />
          <span className="bar" style={{ width: `${18 + ((i * 7) % 14)}%` }} />
          <span className="bar" style={{ width: `${30 + ((i * 11) % 26)}%` }} />
          <span className="bar" style={{ width: "8%" }} />
        </div>
      ))}
    </div>
  );
}
