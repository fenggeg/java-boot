import {useCallback, useEffect, useRef, useState} from "react";
import {Tooltip} from "antd";
import {listen, type UnlistenFn} from "@tauri-apps/api/event";
import {Terminal, type ITheme} from "@xterm/xterm";
import {FitAddon} from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import * as api from "../api";
import {Clear, Restart, X} from "./Icons";

interface Props {
  projectId: string;
}

interface TerminalChunk {
  id: string;
  chunk: string;
  closed: boolean;
}

/** xterm 实例 + 会话 id 的模块级注册表：
 * 面板切换 / 组件重挂载后终端实例与滚动回看均保留（DOM 元素搬移复用） */
const registry = new Map<
  string,
  {term: Terminal; fit: FitAddon; sessionId: string | null}
>();

const FONT_FAMILY =
  'Consolas, "Cascadia Mono", "Courier New", monospace';

function termTheme(dark: boolean): ITheme {
  return dark
    ? {
        background: "#161618",
        foreground: "#d4d4d4",
        cursor: "#d4d4d4",
        selectionBackground: "#3a3d41",
      }
    : {
        background: "#ffffff",
        foreground: "#1f2328",
        cursor: "#1f2328",
        selectionBackground: "#cfe3ff",
      };
}

/**
 * 项目集成终端（ConPTY 全功能模式）。
 * 前端 xterm.js 与后端伪终端直连：键盘输入原样透传（含 Ctrl+C、方向键、
 * PSReadLine 历史），输出按 VT 序列渲染，彩色/交互式程序完整可用；
 * 无独立输入行，点击终端区域即可直接输入。
 */
export default function TerminalView({projectId}: Props) {
  // 惰性创建会话：首次展开抽屉时才拉起 shell 进程
  const [booted, setBooted] = useState(() => registry.has(projectId));
  const [closed, setClosed] = useState(false);
  const [busy, setBusy] = useState(false);
  const hostRef = useRef<HTMLDivElement>(null);
  const sessionIdRef = useRef<string | null>(
    registry.get(projectId)?.sessionId ?? null
  );
  const unlistenRef = useRef<UnlistenFn | null>(null);
  // 用于 attachListener 的 disposed 标记，组件卸载时标记已废弃
  const attachDisposedRef = useRef<{ current: boolean }>({ current: false });

  const isDark = () =>
    document.documentElement.getAttribute("data-theme") === "dark";

  /** 创建或复用 xterm 实例并挂载到容器，返回注册表条目 */
  const ensureTerm = useCallback(
    (): {term: Terminal; fit: FitAddon; sessionId: string | null} => {
      let entry = registry.get(projectId);
      if (!entry) {
        const term = new Terminal({
          fontSize: 12.5,
          lineHeight: 1.3,
          fontFamily: FONT_FAMILY,
          cursorBlink: true,
          scrollback: 5000,
          theme: termTheme(isDark()),
        });
        const fit = new FitAddon();
        term.loadAddon(fit);
        entry = {term, fit, sessionId: null};
        registry.set(projectId, entry);

        // 键盘输入原样透传到 PTY —— 终端内直接打字，无需独立输入行
        term.onData((data) => {
          const sid = registry.get(projectId)?.sessionId;
          if (sid) void api.terminalWrite(sid, data).catch(() => {});
        });
        // 尺寸变化同步到 ConPTY
        term.onResize(({cols, rows}) => {
          const sid = registry.get(projectId)?.sessionId;
          if (sid) void api.terminalResize(sid, cols, rows).catch(() => {});
        });
      }
      const host = hostRef.current;
      if (host && !entry.term.element?.isConnected) {
        // 重挂载：把保留的 xterm DOM 搬回当前容器
        if (entry.term.element) host.appendChild(entry.term.element);
        else entry.term.open(host);
      }
      return entry;
    },
    [projectId]
  );

  /** 订阅输出事件并写入 xterm */
  const attachListener = useCallback(() => {
    unlistenRef.current?.();
    unlistenRef.current = null;
    // 防止 listen resolve 前 cleanup 已执行导致监听泄漏
    const disposedRef = { current: false };
    (async () => {
      const unlisten = await listen<TerminalChunk>("terminal://out", (e) => {
        const {id, chunk, closed: isClosed} = e.payload;
        const entry = registry.get(projectId);
        if (!entry || id !== entry.sessionId) return;
        entry.term.write(chunk);
        if (isClosed && id === entry.sessionId) {
          entry.sessionId = null;
          sessionIdRef.current = null;
          setClosed(true);
        }
      });
      if (disposedRef.current) {
        unlisten();
        return;
      }
      unlistenRef.current = unlisten;
    })();
    // 保存 disposedRef 以便 cleanup 时标记
    attachDisposedRef.current = disposedRef;
  }, [projectId]);

  /** 创建后端会话并绑定到 xterm 实例 */
  const boot = useCallback(async () => {
    const entry = ensureTerm();
    if (entry.term.element) {
      entry.fit.fit();
    }
    attachListener();
    if (entry.sessionId) {
      // 已有活会话：直接复用
      sessionIdRef.current = entry.sessionId;
      setBooted(true);
      setClosed(false);
      return;
    }
    setBusy(true);
    try {
      const sid = await api.terminalCreate(projectId);
      entry.sessionId = sid;
      sessionIdRef.current = sid;
      setClosed(false);
      setBooted(true);
      entry.term.reset();
      entry.term.focus();
      entry.fit.fit();
      await api.terminalResize(sid, entry.term.cols, entry.term.rows).catch(() => {});
    } catch (e) {
      entry.term.write(`\x1b[31m[终端启动失败] ${api.toErrMsg(e)}\x1b[0m\r\n`);
      setClosed(true);
    } finally {
      setBusy(false);
    }
  }, [projectId, ensureTerm, attachListener]);

  // 重启：杀掉旧会话（含进程树）再新建
  const restart = useCallback(async () => {
    const old = registry.get(projectId)?.sessionId;
    if (old) {
      try {
        await api.terminalKill(old);
      } catch {
        /* 会话可能已退出 */
      }
    }
    await boot();
  }, [projectId, boot]);

  // 关闭终端：终止会话进程并回到「启动终端」待机界面
  const closeTerminal = useCallback(async () => {
    const entry = registry.get(projectId);
    const sid = entry?.sessionId ?? null;
    if (entry) {
      entry.sessionId = null;
      entry.term.reset();
    }
    sessionIdRef.current = null;
    if (sid) {
      try {
        // 后端杀整棵进程树（shell 里跑的 mvn 等一并结束）
        await api.terminalKill(sid);
      } catch {
        /* 会话可能已自行退出 */
      }
    }
    setClosed(false);
    setBusy(false);
    setBooted(false);
  }, [projectId]);

  // 首次挂载：恢复既有实例或等待用户手动启动；窗口聚焦时聚焦终端
  useEffect(() => {
    const entry = registry.get(projectId);
    if (entry?.sessionId) {
      ensureTerm();
      attachListener();
      setClosed(false);
      setBooted(true);
      entry.fit.fit();
    }
    return () => {
      attachDisposedRef.current.current = true;
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  // 容器尺寸变化时自适应行列数
  useEffect(() => {
    const host = hostRef.current;
    if (!host || !booted) return;
    const ro = new ResizeObserver(() => {
      const entry = registry.get(projectId);
      if (!entry) return;
      try {
        entry.fit.fit();
      } catch {
        /* 容器不可见时跳过 */
      }
    });
    ro.observe(host);
    return () => ro.disconnect();
  }, [projectId, booted]);

  // 跟随应用明暗主题
  useEffect(() => {
    const ob = new MutationObserver(() => {
      const entry = registry.get(projectId);
      if (entry) entry.term.options.theme = termTheme(isDark());
    });
    ob.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => ob.disconnect();
  }, [projectId]);

  // 点击终端区域获得焦点，直接开始输入
  const focusTerm = useCallback(() => {
    registry.get(projectId)?.term.focus();
  }, [projectId]);

  if (!booted) {
    return (
      <div className="term-boot">
        <button className="term-boot-btn" onClick={() => void boot()} disabled={busy}>
          {busy ? "启动中..." : "启动终端"}
        </button>
        <span className="term-boot-hint">在项目根目录启动 PowerShell 终端</span>
      </div>
    );
  }

  return (
    <div className="term-view">
      <div className="term-toolbar">
        <span className={`term-status ${closed ? "off" : "on"}`}>
          {closed ? "已退出" : busy ? "启动中" : "运行中"}
        </span>
        <div style={{marginLeft: "auto", display: "flex", gap: 4}}>
          <Tooltip title="清屏">
            <button
              className="icon-btn sm"
              onClick={() => registry.get(projectId)?.term.clear()}
              aria-label="清屏"
            >
              <Clear size={13} />
            </button>
          </Tooltip>
          <Tooltip title="重启终端">
            <button
              className="icon-btn sm"
              onClick={() => void restart()}
              aria-label="重启终端"
            >
              <Restart size={13} />
            </button>
          </Tooltip>
          <Tooltip title="关闭终端（结束进程）">
            <button
              className="icon-btn sm"
              onClick={() => void closeTerminal()}
              aria-label="关闭终端"
            >
              <X size={13} />
            </button>
          </Tooltip>
        </div>
      </div>
      <div
        className={`term-host ${closed ? "off" : ""}`}
        ref={hostRef}
        onMouseUp={focusTerm}
      >
        {closed && (
          <div className="term-exit-overlay">
            <span>会话已退出</span>
            <button className="term-boot-btn" onClick={() => void restart()}>
              重新启动
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
