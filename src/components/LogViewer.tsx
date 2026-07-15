import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {Input, Segmented, Tooltip} from "antd";
import {ArrowDown, Clear, Search, Terminal} from "./Icons";
import {useStore} from "../store";
import type {LogLine} from "../types";

interface Props {
  serviceId: string | null;
}

type LogLevel = "all" | "info" | "warn" | "error";

function classifyLine(line: string): "info" | "warn" | "error" {
  const lower = line.toLowerCase();
  if (/\b(error|exception|fail(ed)?|fatal)\b/.test(lower)) return "error";
  if (/\b(warn(ing)?)\b/.test(lower)) return "warn";
  return "info";
}

function matchLevel(line: string, level: LogLevel): boolean {
  if (level === "all") return true;
  return classifyLine(line) === level;
}

function sourceClass(source: string): string {
  if (source.includes("mvn")) return "mvn";
  if (source.includes("git")) return "git";
  return "app";
}

const LINE_HEIGHT = 19;
const OVERSCAN = 30;

export default function LogViewer({ serviceId }: Props) {
  const logs = useStore((s) => (serviceId ? s.logs[serviceId] : undefined));
  const clearLog = useStore((s) => s.clearLog);
  const [autoScroll, setAutoScroll] = useState(true);
  const [search, setSearch] = useState("");
  const [level, setLevel] = useState<LogLevel>("all");
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(600);
  const containerRef = useRef<HTMLDivElement>(null);
  // 切换服务时标记需要跳到底部（内容渲染后再执行真正滚动）
  const pendingJumpBottom = useRef(false);

  const allLines = logs?.lines ?? [];
  const filtered = useMemo<LogLine[]>(() => {
    const searchLower = search.toLowerCase();
    return allLines.filter((l) => {
      if (!matchLevel(l.line, level)) return false;
      if (search && !l.line.toLowerCase().includes(searchLower)) return false;
      return true;
    });
  }, [allLines, level, search]);

  const handleScroll = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    setScrollTop(el.scrollTop);
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setAutoScroll(atBottom);
  }, []);

  // 监听容器尺寸
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setViewportH(el.clientHeight);
    });
    ro.observe(el);
    setViewportH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // 自动滚动到底部 / 切换服务后跳转到底部
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    if (pendingJumpBottom.current) {
      // 切换服务：新内容可能还没渲染完，用 rAF 等下一帧再滚动
      requestAnimationFrame(() => {
        if (containerRef.current) {
          containerRef.current.scrollTop = containerRef.current.scrollHeight;
          setScrollTop(containerRef.current.scrollTop);
        }
        pendingJumpBottom.current = false;
      });
    } else if (autoScroll) {
      el.scrollTop = el.scrollHeight;
      setScrollTop(el.scrollTop);
    }
  }, [filtered.length, autoScroll, serviceId]);

  // 切换服务时重置状态并标记需要跳到底部
  useEffect(() => {
    setAutoScroll(true);
    setLevel("all");
    setSearch("");
    pendingJumpBottom.current = true;
  }, [serviceId]);

  if (!serviceId) {
    return (
      <div className="log-empty">
        <div style={{ textAlign: "center", display: "flex", flexDirection: "column", alignItems: "center", gap: 12 }}>
          <span style={{ color: "var(--text-3)", display: "inline-flex" }}>
            <Terminal size={36} />
          </span>
          <div style={{ fontSize: 13, color: "var(--text-3)" }}>
            选择左侧服务查看日志
          </div>
        </div>
      </div>
    );
  }

  // 虚拟滚动计算
  const totalHeight = filtered.length * LINE_HEIGHT;
  const startIdx = Math.max(0, Math.floor(scrollTop / LINE_HEIGHT) - OVERSCAN);
  const endIdx = Math.min(
    filtered.length,
    Math.ceil((scrollTop + viewportH) / LINE_HEIGHT) + OVERSCAN
  );
  const renderLines = filtered.slice(startIdx, endIdx);
  const offsetY = startIdx * LINE_HEIGHT;

  return (
    <div className="log-panel">
      <div className="log-toolbar">
        <Segmented
          size="small"
          value={level}
          onChange={(v) => setLevel(v as LogLevel)}
          options={[
            { label: "全部", value: "all" },
            { label: "INFO", value: "info" },
            { label: "WARN", value: "warn" },
            { label: "ERROR", value: "error" },
          ]}
        />
        <Input
          size="small"
          allowClear
          prefix={<Search size={13} style={{ color: "var(--text-3)" }} />}
          placeholder="搜索日志..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          style={{ width: 200 }}
        />
        <span className="toolbar-count">
          {filtered.length} / {allLines.length} 行
        </span>
        <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
          {!autoScroll && (
            <Tooltip title="滚动到底部">
              <button
                className="icon-btn sm accent"
                onClick={() => {
                  setAutoScroll(true);
                  if (containerRef.current)
                    containerRef.current.scrollTop =
                      containerRef.current.scrollHeight;
                }}
                aria-label="滚动到底部"
              >
                <ArrowDown size={13} />
              </button>
            </Tooltip>
          )}
          <Tooltip title="清空日志">
            <button
              className="icon-btn sm"
              onClick={() => clearLog(serviceId)}
              aria-label="清空日志"
            >
              <Clear size={13} />
            </button>
          </Tooltip>
        </div>
      </div>
      <div
        className="log-viewer"
        ref={containerRef}
        onScroll={handleScroll}
        style={{ position: "relative" }}
      >
        {renderLines.length === 0 ? (
          <div
            style={{
              color: "var(--text-4)",
              textAlign: "center",
              padding: 40,
              fontFamily: "var(--font-sans)",
              fontSize: 13,
              position: "relative",
              zIndex: 2,
            }}
          >
            暂无日志
          </div>
        ) : (
          <div style={{ height: totalHeight, position: "relative" }}>
            <div style={{ transform: `translateY(${offsetY}px)` }}>
              {renderLines.map((l, i) => {
                const lv = classifyLine(l.line);
                const idx = startIdx + i;
                return (
                  <div
                    key={idx}
                    className={`log-line ${sourceClass(l.source)} ${lv}`}
                    style={{ height: LINE_HEIGHT, minHeight: LINE_HEIGHT }}
                  >
                    <span className="log-time">{l.ts.slice(11, 19)}</span>
                    <span className="log-source">{l.source}</span>
                    {l.line}
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
