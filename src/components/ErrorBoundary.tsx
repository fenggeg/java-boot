import { Component, type ReactNode, type ErrorInfo } from "react";

interface Props {
  children: ReactNode;
  fallback?: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export default class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("ErrorBoundary caught:", error, info);
  }

  render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback;
      }
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
          <button
            className="icon-btn"
            onClick={() => window.location.reload()}
            style={{
              marginTop: 8,
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
      );
    }
    return this.props.children;
  }
}