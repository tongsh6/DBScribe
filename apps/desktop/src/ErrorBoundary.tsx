import { Component, type ErrorInfo, type ReactNode } from "react";

type Props = { children: ReactNode };
type State = { error: Error | null };

/**
 * 根组件兜底：渲染期抛异常时展示可读错误，而不是整窗白屏。
 * 之前的 0.4.0 白屏就是因为没有这层兜底，React 直接把整棵树卸载了。
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("界面渲染失败", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div
        style={{
          minHeight: "100vh",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          gap: 16,
          padding: 32,
          fontFamily: "system-ui, -apple-system, 'Segoe UI', sans-serif",
          color: "#1f2328",
          background: "#f6f8fa",
        }}
      >
        <h1 style={{ margin: 0, fontSize: 20 }}>界面加载失败</h1>
        <p style={{ margin: 0, color: "#57606a" }}>应用遇到了未处理的错误，请重试。</p>
        <pre
          style={{
            maxWidth: 720,
            margin: 0,
            padding: 12,
            overflow: "auto",
            fontSize: 12,
            whiteSpace: "pre-wrap",
            background: "#fff",
            border: "1px solid #d0d7de",
            borderRadius: 8,
          }}
        >
          {error.message}
        </pre>
        <button
          onClick={() => window.location.reload()}
          style={{
            padding: "8px 20px",
            fontSize: 14,
            cursor: "pointer",
            border: "1px solid #d0d7de",
            borderRadius: 8,
            background: "#fff",
          }}
        >
          重新加载
        </button>
      </div>
    );
  }
}
