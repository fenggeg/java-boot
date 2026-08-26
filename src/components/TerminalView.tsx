import {useCallback, useEffect, useRef, useState} from "react";
import {Tooltip} from "antd";
import {listen, type UnlistenFn} from "@tauri-apps/api/event";
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

/** 输出缓冲上限（字符），超出丢弃头部，防止长会话内存膨胀 */
const MAX_BUFFER = 200_000;

// ================================================================
// 会话与输出缓冲注册表（模块级）：面板切换 / 组件重挂载后仍保留
// projectId → sessionId / 输出文本
// ================================================================
const sessionByProject = new Map<string, string>();
const bufferByProject = new Map<string, string>();

/**
 * 项目集成终端（cmd.exe 管道模式）。
 * 输出区只读 + 底部输入行；命令回显由前端本地补齐（无 PTY 时 shell 不回显输入）。
 */
export default function TerminalView({projectId}: Props) {
  // 惰性创建会话：首次展开抽屉时才拉起 cmd 进程
  const [booted, setBooted] = useState(
    () => sessionByProject.has(projectId) || bufferByProject.has(projectId)
  );
  const [sessionId, setSessionId] = useState<string | null>(
    () => sessionByProject.get(projectId) ?? null
  );
  const [closed, setClosed] = useState(false);
  const [text, setText] = useState<string>(() => bufferByProject.get(projectId) ?? "");
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const sessionIdRef = useRef<string | null>(sessionId);
  const pendingRef = useRef("");
  const flushScheduledRef = useRef(false);
  const outputRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const stickBottomRef = useRef(true);
  const historyRef = useRef<string[]>([]);
  const historyIdxRef = useRef(-1);

  const appendBuffer = useCallback(
    (chunk: string) => {
      pendingRef.current += chunk;
      if (flushScheduledRef.current) return;
      flushScheduledRef.current = true;
      requestAnimationFrame(() => {
        flushScheduledRef.current = false;
        const next = (bufferByProject.get(projectId) ?? "") + pendingRef.current;
        pendingRef.current = "";
        const capped =
          next.length > MAX_BUFFER ? next.slice(next.length - MAX_BUFFER) : next;
        bufferByProject.set(projectId, capped);
        setText(capped);
      });
    },
    [projectId]
  );

  // 创建会话（首次展开时）
  const boot = useCallback(async () => {
    if (sessionByProject.has(projectId)) {
      // 已有活会话：直接复用
      const sid = sessionByProject.get(projectId)!;
      sessionIdRef.current = sid;
      setSessionId(sid);
      setBooted(true);
      setClosed(false);
      return;
    }
    setBusy(true);
    try {
      const sid = await api.terminalCreate(projectId);
      sessionByProject.set(projectId, sid);
      bufferByProject.delete(projectId);
      sessionIdRef.current = sid;
      setSessionId(sid);
      setText("");
      setClosed(false);
      setBooted(true);
    } catch (e) {
      appendBuffer(`[终端启动失败] ${String(e)}\r\n`);
    } finally {
      setBusy(false);
    }
  }, [projectId, appendBuffer]);

  // 重启：杀掉旧会话（含进程树）再新建
  const restart = useCallback(async () => {
    const old = sessionByProject.get(projectId);
    if (old) {
      sessionByProject.delete(projectId);
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
    const sid = sessionByProject.get(projectId);
    sessionByProject.delete(projectId);
    bufferByProject.delete(projectId);
    sessionIdRef.current = null;
    pendingRef.current = "";
    if (sid) {
      try {
        // 后端经 Job Object 终止整棵进程树（shell 里跑的 mvn 等一并结束）
        await api.terminalKill(sid);
      } catch {
        /* 会话可能已自行退出 */
      }
    }
    setText("");
    setInput("");
    setClosed(false);
    setBusy(false);
    setBooted(false);
  }, [projectId]);

  // 订阅输出事件（组件级过滤本会话）
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    (async () => {
      unlisten = await listen<TerminalChunk>("terminal://out", (e) => {
        const {id, chunk, closed: isClosed} = e.payload;
        if (id !== sessionIdRef.current) return;
        appendBuffer(chunk);
        if (isClosed && id === sessionByProject.get(projectId)) {
          sessionByProject.delete(projectId);
          setClosed(true);
          sessionIdRef.current = null;
        }
      });
    })();
    return () => unlisten?.();
  }, [projectId, appendBuffer]);

  // 自动滚动到底部（用户上滚时暂停跟随）
  useEffect(() => {
    const el = outputRef.current;
    if (el && stickBottomRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [text]);

  const handleOutputScroll = useCallback(() => {
    const el = outputRef.current;
    if (!el) return;
    stickBottomRef.current =
      el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  }, []);

  const submit = useCallback(async () => {
    const cmd = input;
    if (!cmd.trim()) return;
    const sid = sessionByProject.get(projectId);
    if (!sid) {
      await boot();
      return;
    }
    // 本地回显命令行（shell 无 PTY 不回显输入）
    appendBuffer(`${cmd}\n`);
    historyRef.current.push(cmd);
    if (historyRef.current.length > 100) historyRef.current.shift();
    historyIdxRef.current = -1;
    setInput("");
    try {
      await api.terminalWrite(sid, `${cmd}\r\n`);
    } catch {
      appendBuffer("[写入失败，会话可能已退出]\r\n");
      setClosed(true);
    }
  }, [input, projectId, boot, appendBuffer]);

  const handleInputKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void submit();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        const hist = historyRef.current;
        if (hist.length === 0) return;
        historyIdxRef.current =
          historyIdxRef.current < 0
            ? hist.length - 1
            : Math.max(0, historyIdxRef.current - 1);
        setInput(hist[historyIdxRef.current]!);
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        const hist = historyRef.current;
        if (historyIdxRef.current < 0) return;
        historyIdxRef.current += 1;
        if (historyIdxRef.current >= hist.length) {
          historyIdxRef.current = -1;
          setInput("");
        } else {
          setInput(hist[historyIdxRef.current]!);
        }
      }
    },
    [submit]
  );

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
              onClick={() => {
                bufferByProject.delete(projectId);
                pendingRef.current = "";
                setText("");
              }}
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
        className="term-output"
        ref={outputRef}
        onScroll={handleOutputScroll}
        onMouseUp={() => {
          // 未选中文本时点击输出区聚焦输入行
          if (!window.getSelection()?.toString()) inputRef.current?.focus();
        }}
      >
        <pre className="term-text">{text || "正在连接...\n"}</pre>
      </div>
      <div className="term-input-row">
        <span className="term-prompt">&gt;</span>
        <input
          ref={inputRef}
          className="term-input"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleInputKeyDown}
          placeholder={closed ? "会话已退出，点击 ↻ 重启" : "输入命令，Enter 执行"}
          disabled={closed}
          spellCheck={false}
          autoComplete="off"
        />
      </div>
    </div>
  );
}
