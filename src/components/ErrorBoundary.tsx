import {Component, type ErrorInfo, type ReactNode} from "react";

interface Props {
  children: ReactNode;
  fallback?: ReactNode;
  /** 每当 ErrorBoundary 从错误中恢复时调用 */
  onReset?: () => void;
}

interface State {
  hasError: boolean;
  error: Error | null;
  /** 恢复尝试计数，每次 reset 递增 */
  retryCount: number;
}

export default class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null, retryCount: 0 };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error, retryCount: 0 };
  }

  override componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("ErrorBoundary caught:", error, info);
  }

  /**
   * 尝试恢复：重置 hasError 让子组件重新渲染。
   * 如果错误是子组件渲染时的瞬时状态（如网络抖动导致的 undefined），
   * 重新渲染可能恢复正常。最多自动恢复 2 次，超过后只能手动刷新。
   */
  private handleReset = () => {
    this.setState((prev) => ({
      hasError: false,
      error: null,
      retryCount: prev.retryCount + 1,
    }));
    this.props.onReset?.();
  };

  override render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback;
      }
      const canRetry = this.state.retryCount < 2;
      return (
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            padding: 40,
            color: "var(--text-2)",
            fontFamily: "var(--font-sans)",
            gap: 12,
          }}
        >
          <span style={{ fontSize: 32 }}>⚠️</span>
          <div style={{ fontSize: 15, fontWeight: 600, color: "var(--text)" }}>
            页面发生错误
          </div>
          <div style={{ fontSize: 12, color: "var(--text-3)", maxWidth: 400, textAlign: "center" }}>
            {this.state.error?.message}
          </div>
          <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
            {canRetry && (
              <button
                className="icon-btn"
                onClick={this.handleReset}
                style={{
                  padding: "6px 16px",
                  fontSize: 13,
                  background: "var(--surface-2)",
                  color: "var(--text)",
                  border: "1px solid var(--border-2)",
                  borderRadius: "var(--r-sm)",
                  cursor: "pointer",
                }}
              >
                尝试恢复
              </button>
            )}
            <button
              className="icon-btn"
              onClick={() => window.location.reload()}
              style={{
                padding: "6px 16px",
                fontSize: 13,
                background: "var(--blue)",
                color: "#fff",
                border: "none",
                borderRadius: "var(--r-sm)",
                cursor: "pointer",
              }}
            >
              重新加载
            </button>
          </div>
          {canRetry && (
            <div style={{ fontSize: 11, color: "var(--text-3)" }}>
              尝试恢复会重置组件状态（不刷新页面），重新加载会刷新整个页面
            </div>
          )}
        </div>
      );
    }
    return this.props.children;
  }
}