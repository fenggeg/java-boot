import {useCallback, useEffect, useMemo, useState} from "react";
import {App, Button, Empty, Input, Segmented, Spin, Tooltip} from "antd";
import dayjs from "dayjs";
import {Check, ChevronLeft, Commit, GitBranch, GitPull, Plus, Refresh,} from "./Icons";
import * as api from "../api";
import type {GitChange, GitCommitInfo, GitStatus, Project} from "../types";
import {gitChangeKind} from "../types";
import GitDiffModal from "./GitDiffModal";

interface Props {
  project: Project;
  onClose: () => void;
}

const KIND_META: Record<string, { label: string; color: string; bg: string }> = {
  added: { label: "新增", color: "#1d7f3c", bg: "rgba(29,127,60,0.12)" },
  modified: { label: "修改", color: "#b46a00", bg: "rgba(180,106,0,0.12)" },
  deleted: { label: "删除", color: "#d13438", bg: "rgba(209,52,56,0.12)" },
  renamed: { label: "重命名", color: "#7a3ed0", bg: "rgba(122,62,208,0.12)" },
  untracked: { label: "未跟踪", color: "#86868b", bg: "rgba(134,134,139,0.12)" },
  conflict: { label: "冲突", color: "#c50f1f", bg: "rgba(197,15,31,0.12)" },
};

export default function GitPanel({ project, onClose }: Props) {
  const { message, modal } = App.useApp();
  const [tab, setTab] = useState<"changes" | "history">("changes");
  const [status, setStatus] = useState<GitStatus | null>(null);
  const [commits, setCommits] = useState<GitCommitInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [pulling, setPulling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [commitMsg, setCommitMsg] = useState("");
  const [committing, setCommitting] = useState(false);
  const [diffTarget, setDiffTarget] = useState<GitChange | null>(null);
  const [expandedHash, setExpandedHash] = useState<string | null>(null);
  const [showDiff, setShowDiff] = useState<string>("");
  const [diffError, setDiffError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [st, lg] = await Promise.all([
        api.gitStatus(project.id),
        api.gitLog(project.id, 50),
      ]);
      setStatus(st);
      setCommits(lg);
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [project.id]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const stagedChanges = useMemo(
    () => (status?.changes ?? []).filter((c) => c.staged),
    [status]
  );
  const unstagedChanges = useMemo(
    () => (status?.changes ?? []).filter((c) => !c.staged),
    [status]
  );

  const handleStage = async (paths: string[]) => {
    if (paths.length === 0) return;
    try {
      await api.gitStage(project.id, paths);
      await refresh();
    } catch (e: any) {
      message.error(`暂存失败: ${e}`);
    }
  };

  const handleUnstage = async (paths: string[]) => {
    if (paths.length === 0) return;
    try {
      await api.gitUnstage(project.id, paths);
      await refresh();
    } catch (e: any) {
      message.error(`取消暂存失败: ${e}`);
    }
  };

  const handleCommit = async () => {
    const msg = commitMsg.trim();
    if (!msg) return;
    modal.confirm({
      title: "确认提交",
      content: `将暂存的 ${stagedChanges.length} 个文件提交到 ${status?.branch ?? "当前分支"}？`,
      okText: "提交",
      cancelText: "取消",
      onOk: async () => {
        setCommitting(true);
        try {
          await api.gitCommit(project.id, msg);
          message.success("提交成功");
          setCommitMsg("");
          setExpandedHash(null);
          await refresh();
        } catch (e: any) {
          message.error(`提交失败: ${e}`);
          // 抛出异常让 Popconfirm/Modal 保持打开以便用户修正
          throw e;
        } finally {
          setCommitting(false);
        }
      },
    });
  };

  const handlePull = async () => {
    setPulling(true);
    try {
      const r = await api.gitPull(project.id);
      if (r.success) {
        message.success(
          r.up_to_date ? "已是最新" : "拉取成功，工作区已更新"
        );
      } else {
        message.error(`拉取失败: ${r.message}`);
      }
      await refresh();
    } catch (e: any) {
      message.error(`拉取失败: ${e}`);
    } finally {
      setPulling(false);
    }
  };

  const toggleHistoryDiff = async (hash: string) => {
    if (expandedHash === hash) {
      setExpandedHash(null);
      setShowDiff("");
      setDiffError(null);
      return;
    }
    setExpandedHash(hash);
    setShowDiff("");
    setDiffError(null);
    try {
      const d = await api.gitShow(project.id, hash);
      setShowDiff(d);
    } catch (e: unknown) {
      setDiffError(e instanceof Error ? e.message : String(e));
    }
  };

  const renderChangeRow = (c: GitChange) => {
    const kind = gitChangeKind(c);
    const meta = KIND_META[kind] ?? { label: "?", color: "#86868b", bg: "rgba(134,134,139,0.12)" };
    const name = c.old_path ? `${c.old_path} → ${c.path}` : c.path;
    return (
      <div
        key={c.path}
        className="git-change-item"
        onClick={() => setDiffTarget(c)}
        title="点击查看差异"
      >
        <span
          className="git-change-badge"
          style={{ color: meta.color, background: meta.bg }}
        >
          {meta.label}
        </span>
        <span className="git-change-path" title={name}>
          {name}
        </span>
        <span
          style={{ marginLeft: "auto", display: "flex", gap: 4 }}
          onClick={(e) => e.stopPropagation()}
        >
          {c.staged ? (
            <Tooltip title="取消暂存">
              <button
                className="icon-btn sm"
                onClick={() => handleUnstage([c.path])}
                aria-label="取消暂存"
              >
                <Check size={12} />
              </button>
            </Tooltip>
          ) : (
            <Tooltip title="暂存">
              <button
                className="icon-btn sm accent"
                onClick={() => handleStage([c.path])}
                aria-label="暂存"
              >
                <Plus size={12} />
              </button>
            </Tooltip>
          )}
        </span>
      </div>
    );
  };

  const renderChangeSection = (
    title: string,
    list: GitChange[],
    empty: string
  ) => (
    <div className="git-change-section">
      <div className="git-change-section-title">
        {title}
        <span className="git-change-count">{list.length}</span>
      </div>
      {list.length === 0 ? (
        <div className="git-change-empty">{empty}</div>
      ) : (
        list.map(renderChangeRow)
      )}
    </div>
  );

  const hasStaged = stagedChanges.length > 0;

  return (
    <div className="git-panel">
      <div className="git-toolbar">
        <div className="git-toolbar-title">
          <span className="group-icon" style={{ color: "#0071e3", flexShrink: 0 }}>
            <GitBranch size={14} />
          </span>
          <span className="group-name" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{project.name}</span>
          {status?.branch && (
            <span className="git-branch" title="当前分支">
              {status.branch}
              {status.ahead > 0 && (
                <span className="git-branch-count">↑{status.ahead}</span>
              )}
              {status.behind > 0 && (
                <span className="git-branch-count">↓{status.behind}</span>
              )}
            </span>
          )}
        </div>
        <Segmented
          size="small"
          value={tab}
          onChange={(v) => setTab(v as "changes" | "history")}
          options={[
            { label: `改动 ${status?.changes?.length ?? 0}`, value: "changes" },
            { label: "提交历史", value: "history" },
          ]}
        />
        <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
          <Tooltip title="拉取最新代码">
            <button
              className="icon-btn sm accent"
              onClick={handlePull}
              disabled={pulling}
              aria-label="拉取"
            >
              <GitPull size={13} />
            </button>
          </Tooltip>
          <Tooltip title="刷新">
            <button
              className="icon-btn sm"
              onClick={refresh}
              disabled={loading}
              aria-label="刷新"
            >
              <Refresh size={13} />
            </button>
          </Tooltip>
          <Tooltip title="返回日志">
            <button
              className="icon-btn sm"
              onClick={onClose}
              aria-label="返回日志"
            >
              <ChevronLeft size={13} />
            </button>
          </Tooltip>
        </div>
      </div>

      {loading && !status ? (
        <div style={{ padding: 60, textAlign: "center" }}>
          <Spin />
        </div>
      ) : error ? (
        <div style={{ padding: 40 }}>
          <Empty description={error} />
        </div>
      ) : tab === "changes" ? (
        <div className="git-changes">
          <div className="git-changes-actions">
            <span className="toolbar-count">
              共 {(status?.changes?.length ?? 0)} 个改动，
              {hasStaged ? `${stagedChanges.length} 个已暂存` : "尚未暂存"}
            </span>
            <span style={{ display: "flex", gap: 6 }}>
              <Button
                size="small"
                icon={<Plus size={12} />}
                onClick={() =>
                  handleStage(unstagedChanges.map((c) => c.path))
                }
                disabled={unstagedChanges.length === 0}
              >
                全部暂存
              </Button>
              <Button
                size="small"
                icon={<Check size={12} />}
                onClick={() => handleUnstage(stagedChanges.map((c) => c.path))}
                disabled={!hasStaged}
              >
                取消暂存
              </Button>
            </span>
          </div>

          {renderChangeSection("已暂存", stagedChanges, "暂存区为空")}
          {renderChangeSection(
            "工作区改动",
            unstagedChanges,
            "工作区干净，没有待提交的改动"
          )}

          <div className="git-commit-box">
            <Input.TextArea
              value={commitMsg}
              onChange={(e) => setCommitMsg(e.target.value)}
              placeholder="提交信息（将提交暂存区的文件）"
              autoSize={{ minRows: 2, maxRows: 5 }}
              disabled={committing}
            />
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                marginTop: 8,
              }}
            >
              <span className="toolbar-count">
                将提交 {stagedChanges.length} 个文件
              </span>
              <Button
                type="primary"
                size="small"
                icon={<Commit size={12} />}
                onClick={handleCommit}
                loading={committing}
                disabled={!hasStaged || !commitMsg.trim()}
              >
                提交
              </Button>
            </div>
          </div>
        </div>
      ) : (
        <div className="git-history">
          {commits.length === 0 ? (
            <Empty description="暂无提交记录" style={{ marginTop: 40 }} />
          ) : (
            commits.map((c) => (
              <div key={c.hash} className="git-history-item">
                <div
                  className="git-history-row"
                  onClick={() => toggleHistoryDiff(c.hash)}
                >
                  <span className="git-history-hash" title={c.hash}>
                    {c.short_hash}
                  </span>
                  <span className="git-history-msg">{c.message}</span>
                  <span className="git-history-meta">
                    {c.author} · {dayjs(c.date).format("YYYY-MM-DD HH:mm")}
                  </span>
                  {expandedHash === c.hash ? (
                    <ChevronLeft
                      size={12}
                      style={{ transform: "rotate(-90deg)", color: "var(--text-3)" }}
                    />
                  ) : (
                    <ChevronLeft
                      size={12}
                      style={{ transform: "rotate(90deg)", color: "var(--text-3)" }}
                    />
                  )}
                </div>
                {expandedHash === c.hash && (
                  <div className="git-history-diff">
                    {diffError ? (
                      <div style={{ padding: 16, color: "#ff3b30", fontSize: 12 }}>
                        加载 diff 失败: {diffError}
                      </div>
                    ) : showDiff ? (
                      <pre className="git-history-diff-pre">{showDiff}</pre>
                    ) : (
                      <Spin size="small" />
                    )}
                  </div>
                )}
              </div>
            ))
          )}
        </div>
      )}

      <GitDiffModal
        projectId={project.id}
        change={diffTarget}
        onClose={() => setDiffTarget(null)}
        onChanged={refresh}
      />
    </div>
  );
}