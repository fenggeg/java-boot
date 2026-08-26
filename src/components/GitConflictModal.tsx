import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {App, Button, Modal, Spin} from "antd";
import {Check, Warning} from "./Icons";
import * as api from "../api";

interface Props {
  projectId: string;
  /** 冲突文件路径列表（父级随状态刷新更新） */
  paths: string[];
  /** 是否处于合并中（全部解决后用于显示「完成合并」） */
  merging: boolean;
  open: boolean;
  onClose: () => void;
  onChanged: () => Promise<void>;
}

/**
 * 行级 LCS 差集：返回 side 中与 base 不同的行下标集合。
 * 超大文件（>1500 行）跳过对齐直接整文件标红，避免 DP 内存爆炸。
 */
function changedLines(base: string[], side: string[]): Set<number> {
  const CAP = 1500;
  if (base.length > CAP || side.length > CAP) {
    return new Set(side.map((_, i) => i));
  }
  const m = base.length;
  const n = side.length;
  const dp: Uint32Array[] = Array.from({length: m + 1}, () => new Uint32Array(n + 1));
  for (let i = m - 1; i >= 0; i--) {
    for (let j = n - 1; j >= 0; j--) {
      dp[i]![j] =
        base[i] === side[j]
          ? dp[i + 1]![j + 1]! + 1
          : Math.max(dp[i + 1]![j]!, dp[i]![j + 1]!);
    }
  }
  const res = new Set<number>();
  let i = 0;
  let j = 0;
  while (i < m && j < n) {
    if (base[i] === side[j]) {
      i++;
      j++;
    } else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) {
      i++;
    } else {
      res.add(j);
      j++;
    }
  }
  while (j < n) {
    res.add(j);
    j++;
  }
  return res;
}

/** 去掉末尾因换行产生的空串 */
function toLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/**
 * 冲突合并弹窗（参照 IDEA 三栏布局）：
 * 左=本地修改(Yours) 中=合并结果(可编辑) 右=远程修改(Theirs)
 * - 采用本地/远程/双侧：立即写入并标记该文件已解决
 * - 中间栏可直接编辑，点「标记已解决」保存内容
 * - 全部解决后「完成合并提交」；随时可「中止合并」
 */
export default function GitConflictModal({
  projectId,
  paths,
  merging,
  open,
  onClose,
  onChanged,
}: Props) {
  const {message, modal} = App.useApp();
  // 本次会话已解决的文件（父级刷新前列表可能滞后，本地先行剔除）
  const [resolvedLocal, setResolvedLocal] = useState<Set<string>>(new Set());
  const [activePath, setActivePath] = useState<string | null>(null);
  const [versions, setVersions] = useState<api.ConflictVersions | null>(null);
  const [loadingVer, setLoadingVer] = useState(false);
  /** 中间栏可编辑的合并结果 */
  const [resultText, setResultText] = useState("");
  /** resultText 当前归属的文件路径（切换文件时丢弃未保存草稿） */
  const loadedForRef = useRef<string | null>(null);
  const [busy, setBusy] = useState(false);

  const remaining = useMemo(
    () => paths.filter((p) => !resolvedLocal.has(p)),
    [paths, resolvedLocal]
  );

  // 打开时重置会话内状态
  useEffect(() => {
    if (!open) return;
    setResolvedLocal(new Set());
    setActivePath(paths[0] ?? null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 切换活动文件时加载三方版本与当前工作区内容（含 git 冲突标记）
  useEffect(() => {
    if (!open || !activePath) return;
    let cancelled = false;
    setLoadingVer(true);
    setVersions(null);
    (async () => {
      try {
        const v = await api.gitConflictVersions(projectId, activePath);
        if (cancelled) return;
        setVersions(v);
        let content = v.ours;
        try {
          content = await api.gitReadFile(projectId, activePath);
        } catch {
          /* 工作区读取失败时回退 ours 内容 */
        }
        setResultText(content);
        loadedForRef.current = activePath;
      } catch (e: any) {
        if (!cancelled) message.error(`读取冲突版本失败: ${e}`);
      } finally {
        if (!cancelled) setLoadingVer(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, activePath, projectId, message]);

  /** 解决完当前文件后跳到下一个未解决文件；全部解决则停在完成页 */
  const advance = useCallback(
    (justResolved: string) => {
      setResolvedLocal((prev) => new Set(prev).add(justResolved));
      const rest = remaining.filter((p) => p !== justResolved);
      setActivePath(rest[0] ?? null);
    },
    [remaining]
  );

  const doResolveSide = useCallback(
    async (side: "ours" | "theirs" | "both") => {
      if (!activePath) return;
      setBusy(true);
      try {
        await api.gitResolveSide(projectId, activePath, side);
        message.success(
          side === "ours"
            ? "已采用本地修改"
            : side === "theirs"
              ? "已采用远程修改"
              : "已合并双侧修改"
        );
        advance(activePath);
        await onChanged();
      } catch (e: any) {
        message.error(`解决失败: ${e}`);
      } finally {
        setBusy(false);
      }
    },
    [activePath, projectId, advance, onChanged, message]
  );

  /** 把中间栏编辑后的内容写回并标记已解决 */
  const doMarkResolved = useCallback(async () => {
    if (!activePath) return;
    setBusy(true);
    try {
      await api.gitMarkResolved(projectId, activePath, resultText);
      message.success("已标记为已解决");
      advance(activePath);
      await onChanged();
    } catch (e: any) {
      message.error(`标记失败: ${e}`);
    } finally {
      setBusy(false);
    }
  }, [activePath, projectId, resultText, advance, onChanged, message]);

  const doAbort = useCallback(() => {
    modal.confirm({
      title: "中止本次合并？",
      content: "工作区将恢复到合并前的状态，所有解决进度将被丢弃。",
      okText: "中止合并",
      okButtonProps: {danger: true},
      cancelText: "取消",
      onOk: async () => {
        setBusy(true);
        try {
          await api.gitAbortMerge(projectId);
          message.success("已中止合并");
          setResolvedLocal(new Set());
          setActivePath(null);
          onClose();
          await onChanged();
        } catch (e: any) {
          message.error(`中止失败: ${e}`);
        } finally {
          setBusy(false);
        }
      },
    });
  }, [projectId, onClose, onChanged, message, modal]);

  const doComplete = useCallback(async () => {
    setBusy(true);
    try {
      // 沿用 git 生成的默认合并信息（Merge branch ...）
      await api.gitCompleteMerge(projectId, null);
      message.success("合并已完成并提交");
      setResolvedLocal(new Set());
      setActivePath(null);
      onClose();
      await onChanged();
    } catch (e: any) {
      message.error(`完成合并失败: ${e}`);
    } finally {
      setBusy(false);
    }
  }, [projectId, onClose, onChanged, message]);

  const baseLines = useMemo(() => toLines(versions?.base ?? ""), [versions]);
  const oursChanged = useMemo(
    () => changedLines(baseLines, toLines(versions?.ours ?? "")),
    [baseLines, versions]
  );
  const theirsChanged = useMemo(
    () => changedLines(baseLines, toLines(versions?.theirs ?? "")),
    [baseLines, versions]
  );

  const renderPaneHeader = (label: string, hint?: string) => (
    <div className="merge-pane-head">
      <span>{label}</span>
      {hint && <span className="merge-pane-hint">{hint}</span>}
    </div>
  );

  const renderSidePane = (
    text: string | undefined,
    changed: Set<number>,
    tone: "ours" | "theirs"
  ) => (
    <pre className={`merge-pane merge-pane-${tone}`}>
      {text === undefined ? null : toLines(text).map((l, i) => (
        <div key={i} className={changed.has(i) ? "ln-changed" : undefined}>
          {l || "\u00A0"}
        </div>
      ))}
    </pre>
  );

  const allResolved = merging && remaining.length === 0;

  return (
    <Modal
      title={
        <span style={{display: "inline-flex", alignItems: "center", gap: 8}}>
          <Warning size={15} style={{color: "#ff9500"}} />
          解决合并冲突
          {!allResolved && (
            <span className="merge-count">{remaining.length} 个待解决</span>
          )}
        </span>
      }
      open={open}
      onCancel={busy ? undefined : onClose}
      width={1080}
      footer={
        busy ? (
          <Spin size="small" />
        ) : allResolved ? (
          <>
            <Button onClick={onClose}>稍后处理</Button>
            <Button type="primary" icon={<Check size={13} />} onClick={doComplete}>
              完成合并提交
            </Button>
          </>
        ) : (
          <>
            <Button danger onClick={doAbort} disabled={busy}>
              中止合并
            </Button>
            <Button
              type="primary"
              icon={<Check size={13} />}
              disabled={!activePath || loadingVer}
              onClick={doMarkResolved}
            >
              标记已解决并暂存
            </Button>
          </>
        )
      }
      destroyOnClose
    >
      {allResolved ? (
        <div className="merge-all-done">
          <Check size={36} />
          <span>所有冲突已解决，点击「完成合并提交」结束本次合并</span>
        </div>
      ) : (
        <div className="merge-layout">
          {/* 文件列表 */}
          <div className="merge-files">
            {paths.map((p) => {
              const done = resolvedLocal.has(p);
              return (
                <div
                  key={p}
                  className={`merge-file-item ${p === activePath ? "active" : ""}`}
                  onClick={() => !done && setActivePath(p)}
                  title={done ? "已解决" : p}
                >
                  <span className={`merge-file-dot ${done ? "done" : "todo"}`} />
                  <span className={`merge-file-name ${done ? "done" : ""}`}>{p}</span>
                  {done && <Check size={12} style={{color: "#34c759", flexShrink: 0}} />}
                </div>
              );
            })}
          </div>

          {/* 三栏对比 */}
          <div className="merge-panes">
            <div className="merge-toolbar">
              <span className="toolbar-count">
                {activePath ?? "—"}
              </span>
              <span style={{display: "flex", gap: 6}}>
                <Button
                  size="small"
                  disabled={!activePath || loadingVer || busy}
                  onClick={() => void doResolveSide("ours")}
                >
                  采用本地
                </Button>
                <Button
                  size="small"
                  disabled={!activePath || loadingVer || busy}
                  onClick={() => void doResolveSide("theirs")}
                >
                  采用远程
                </Button>
                <Button
                  size="small"
                  disabled={!activePath || loadingVer || busy}
                  onClick={() => void doResolveSide("both")}
                >
                  双侧合并
                </Button>
              </span>
            </div>
            {loadingVer ? (
              <div className="merge-loading">
                <Spin />
              </div>
            ) : (
              <div className="merge-grid">
                {renderPaneHeader("本地修改 (Yours)", "只读")}
                {renderPaneHeader("合并结果", "可直接编辑")}
                {renderPaneHeader("远程修改 (Theirs)", "只读")}
                {renderSidePane(versions?.ours, oursChanged, "ours")}
                <textarea
                  className="merge-result"
                  value={resultText}
                  onChange={(e) => setResultText(e.target.value)}
                  spellCheck={false}
                  disabled={!activePath || busy}
                />
                {renderSidePane(versions?.theirs, theirsChanged, "theirs")}
              </div>
            )}
          </div>
        </div>
      )}
    </Modal>
  );
}
