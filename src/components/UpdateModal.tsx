import {useCallback, useEffect, useRef, useState} from "react";
import {App, Button, Modal, Progress, Spin} from "antd";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {ArrowDown, Check, Download, Refresh, Warning} from "./Icons";
import {checkForUpdate, downloadAndInstall, cancelUpdate, formatSize, relaunchAndInstall, type UpdateInfo,} from "../update";
import {toErrMsg} from "../api";

interface Props {
  open: boolean;
  onClose: () => void;
}

/**
 * 检查阶段：
 *  checking   检查中
 *  available  有可用更新（展示更新日志 + 立即更新/取消）
 *  latest     已是最新版本
 *  error      检查失败（可重试）
 */
type Phase = "checking" | "available" | "latest" | "error";

export default function UpdateModal({open, onClose}: Props) {
  const {message} = App.useApp();
  const [phase, setPhase] = useState<Phase>("checking");
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState(0);
  /** 实时速度（字节/秒） */
  const [speed, setSpeed] = useState(0);
  /** 已下载 / 总字节数 */
  const [dlBytes, setDlBytes] = useState(0);
  const [totalBytes, setTotalBytes] = useState(0);
  const [downloaded, setDownloaded] = useState(false);
  // 已下载的安装包路径（重启安装用）
  const installerPathRef = useRef<string | null>(null);

  // ---- 检查更新（打开时自动执行，失败可重试） ----
  const runCheck = useCallback((cancelledRef: {value: boolean}) => {
    setPhase("checking");
    setInfo(null);
    setError(null);
    checkForUpdate()
      .then((result) => {
        if (cancelledRef.value) return;
        setInfo(result);
        setPhase(result.available ? "available" : "latest");
      })
      .catch((e) => {
        if (cancelledRef.value) return;
        setError(toErrMsg(e));
        setPhase("error");
      });
  }, []);

  useEffect(() => {
    if (!open) return;
    setDownloading(false);
    setProgress(0);
    setSpeed(0);
    setDlBytes(0);
    setTotalBytes(0);
    setDownloaded(false);
    installerPathRef.current = null;
    const cancelledRef = {value: false};
    runCheck(cancelledRef);
    return () => {
      cancelledRef.value = true;
    };
  }, [open, runCheck]);

  // ---- 立即更新（后端流式下载，进度经 update://progress 事件上报） ----
  const handleUpdate = useCallback(async () => {
    if (!info) return;
    setDownloading(true);
    setProgress(0);
    setSpeed(0);
    setDlBytes(0);
    try {
      installerPathRef.current = await downloadAndInstall(
        info.download_url,
        (p) => {
          setProgress(p.percent);
          setSpeed(p.speed);
          setDlBytes(p.downloaded);
          setTotalBytes(p.total);
        }
      );
      setDownloaded(true);
    } catch (e) {
      // 取消不弹错误提示，其他失败才提示
      if (!toErrMsg(e).includes("下载已取消")) {
        message.error(`下载失败: ${toErrMsg(e)}`);
      }
    } finally {
      setDownloading(false);
    }
  }, [info, message]);

  // ---- 取消下载：通知后端中止，downloadAndInstall 的 Promise 会 reject ----
  const handleCancelDownload = useCallback(async () => {
    try {
      await cancelUpdate();
    } catch {
      // 后端无下载任务或已结束，忽略
    }
  }, []);

  // ---- 立即重启：静默安装器启动后当前进程退出，后续代码不会执行 ----
  const handleRelaunch = useCallback(async () => {
    if (!installerPathRef.current) {
      message.error("尚未下载更新包");
      return;
    }
    try {
      await relaunchAndInstall(installerPathRef.current);
    } catch (e) {
      message.error(`重启失败: ${toErrMsg(e)}`);
    }
  }, [message]);

  const renderMarkdown = (notes: string) => (
    <div className="file-preview-markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
      >
        {notes}
      </ReactMarkdown>
    </div>
  );

  // ---- 弹窗内容 ----
  let content: React.ReactNode;
  switch (phase) {
    case "checking":
      content = (
        <div className="update-center">
          <Spin />
          <span>正在检查更新...</span>
        </div>
      );
      break;
    case "latest":
      content = (
        <div className="update-center">
          <span className="update-status-ok">
            <Check size={40} />
          </span>
          <span>当前已是最新版本</span>
          {info && (
            <span className="update-meta">
              v{info.current_version}
            </span>
          )}
        </div>
      );
      break;
    case "error":
      content = (
        <div className="update-center">
          <span className="update-status-err">
            <Warning size={36} />
          </span>
          <span>检查更新失败</span>
          {error && (
            <span className="update-meta" style={{maxWidth: 380, wordBreak: "break-all"}}>
              {error}
            </span>
          )}
          <Button
            size="small"
            icon={<Refresh size={13} />}
            onClick={() => runCheck({value: false})}
          >
            重试
          </Button>
        </div>
      );
      break;
    default:
      content = info ? (
        <>
          <div className="update-version-row">
            <span className="update-version-pill">
              v{info.current_version}
            </span>
            <span className="update-version-arrow">
              <ArrowDown size={14} style={{transform: "rotate(-90deg)"}} />
            </span>
            <span className="update-version-pill new">v{info.version}</span>
            <span className="update-meta">
              {[info.pub_date, info.download_size].filter(Boolean).join(" · ")}
            </span>
          </div>

          <div className="update-changelog">{renderMarkdown(info.notes)}</div>

          {downloading && (
            <div className="update-progress-row">
              <Progress
                percent={progress}
                size="small"
                strokeColor="var(--blue)"
              />
              <span className="update-meta update-speed" title="实时下载速度">
                {totalBytes > 0
                  ? `${formatSize(dlBytes)} / ${formatSize(totalBytes)}`
                  : formatSize(dlBytes)}
                {" · "}
                {formatSize(speed)}/s
              </span>
            </div>
          )}
          {downloaded && (
            <div className="update-progress-row">
              <span className="update-status-ok">
                <Check size={14} />
              </span>
              <span className="update-meta">下载完成，重启应用以完成安装</span>
            </div>
          )}
        </>
      ) : null;
  }

  // ---- 底部按钮 ----
  // 下载中显示"取消"按钮：触发后端中止下载
  let footer: React.ReactNode = null;
  if (phase === "available" && info) {
    if (downloaded) {
      footer = (
        <>
          <Button onClick={onClose}>以后再说</Button>
          <Button type="primary" icon={<Refresh size={13} />} onClick={handleRelaunch}>
            立即重启
          </Button>
        </>
      );
    } else if (downloading) {
      footer = (
        <Button danger onClick={handleCancelDownload}>
          取消下载
        </Button>
      );
    } else {
      footer = (
        <>
          <Button onClick={onClose}>取消</Button>
          <Button type="primary" icon={<Download size={13} />} onClick={handleUpdate}>
            立即更新
          </Button>
        </>
      );
    }
  } else if (phase === "latest" || phase === "error") {
    footer = (
      <Button type="primary" onClick={onClose}>
        知道了
      </Button>
    );
  }

  return (
    <Modal
      title="软件更新"
      open={open}
      onCancel={onClose}
      footer={footer}
      width={560}
      maskClosable={!downloading}
      keyboard={!downloading}
      closable={!downloading}
      destroyOnClose
    >
      {content}
    </Modal>
  );
}
