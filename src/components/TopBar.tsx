import {useCallback, useEffect, useMemo, useState} from "react";
import type {UnlistenFn} from "@tauri-apps/api/event";
import {getCurrentWindow} from "@tauri-apps/api/window";
import {App, Badge, Button, Popconfirm, Tooltip} from "antd";
import {Copy, Minus, Moon, Settings, Square, Stop, Sun, Update, Warning, X} from "./Icons";
import {useStore} from "../store";
import * as api from "../api";
import {STATUS_META} from "../types";
import {useThemeStore} from "../theme";
import {checkForUpdate} from "../update";
import UpdateModal from "./UpdateModal";

interface Props {
  onOpenSettings: () => void;
}

export default function TopBar({ onOpenSettings }: Props) {
  const services = useStore((s) => s.services);
  const runtimes = useStore((s) => s.runtimes);
  const refreshServices = useStore((s) => s.refreshServices);
  const { message } = App.useApp();
  const themeMode = useThemeStore((s) => s.mode);
  const toggleTheme = useThemeStore((s) => s.toggle);
  const [hoverTheme, setHoverTheme] = useState(false);
  // 检查更新弹窗
  const [updateOpen, setUpdateOpen] = useState(false);
  // 自动检测到的新版本（小红点）
  const [hasUpdate, setHasUpdate] = useState(false);

  // ---- 自动检查更新：启动 3 秒后首查，之后每 4 小时复查；失败静默忽略 ----
  const silentCheck = useCallback(async () => {
    try {
      const r = await checkForUpdate();
      setHasUpdate(r.available);
    } catch {
      /* 网络异常静默忽略，不打扰用户 */
    }
  }, []);

  useEffect(() => {
    const t = window.setTimeout(() => void silentCheck(), 3000);
    const iv = window.setInterval(() => void silentCheck(), 4 * 60 * 60 * 1000);
    return () => {
      window.clearTimeout(t);
      window.clearInterval(iv);
    };
  }, [silentCheck]);

  // 自定义窗口控制：最小化 / 最大化还原 / 关闭
  const appWindow = useMemo(() => getCurrentWindow(), []);
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
    appWindow.isMaximized().then((v) => {
      if (!disposed) setIsMaximized(v);
    }).catch(() => {});
    appWindow.onResized(async () => {
      try {
        const v = await appWindow.isMaximized();
        if (!disposed) setIsMaximized(v);
      } catch {
        /* ignore */
      }
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [appWindow]);

  const total = services.length;
  const { running, errorCount } = useMemo(() => {
    let r = 0;
    let e = 0;
    for (const s of services) {
      const st = runtimes[s.id]?.status;
      if (st === "running") r++;
      else if (st === "error") e++;
    }
    return { running: r, errorCount: e };
  }, [services, runtimes]);

  const handleStopAll = async () => {
    try {
      await api.stopAll();
      message.success("已停止所有运行中的服务");
      await refreshServices();
    } catch (e) {
      message.error(`停止失败: ${api.toErrMsg(e)}`);
    }
  };

  return (
    <div className="topbar" data-tauri-drag-region>
      <div className="topbar-stats" data-tauri-drag-region>
        <span className="stat-item">
          <span
            className="stat-dot"
            style={{ background: STATUS_META.running.dot, color: STATUS_META.running.dot }}
          />
          <span className="stat-label">运行</span>
          <span className="stat-val">{running}</span>
          <span className="stat-label">/ {total}</span>
        </span>
        {errorCount > 0 && (
          <span className="stat-item">
            <span
              className="stat-dot"
              style={{ background: STATUS_META.error.dot, color: STATUS_META.error.dot }}
            />
            <span className="stat-label">异常</span>
            <span className="stat-val" style={{ color: "#ff3b30" }}>{errorCount}</span>
          </span>
        )}
      </div>

      <div className="topbar-actions">
        {running > 0 && (
          <Popconfirm
            title="停止所有运行中的服务？"
            description={`将强制结束 ${running} 个运行中的服务`}
            icon={<Warning size={14} style={{ color: "var(--red)" }} />}
            onConfirm={handleStopAll}
            okText="停止全部"
            cancelText="取消"
            okButtonProps={{ danger: true, size: "small" }}
            cancelButtonProps={{ size: "small" }}
            placement="bottomRight"
          >
            <Button size="small" danger icon={<Stop size={13} />}>
              停止全部
            </Button>
          </Popconfirm>
        )}

        <Tooltip title={themeMode === "light" ? "切换到暗色模式" : "切换到亮色模式"}>
          <button
            className="icon-btn"
            onClick={toggleTheme}
            onMouseEnter={() => setHoverTheme(true)}
            onMouseLeave={() => setHoverTheme(false)}
            aria-label="切换主题"
          >
            {themeMode === "light" ? (
              <Moon size={15} style={{ color: hoverTheme ? "#0071e3" : undefined }} />
            ) : (
              <Sun size={15} style={{ color: hoverTheme ? "#ffd60a" : undefined }} />
            )}
          </button>
        </Tooltip>

        <Tooltip title={hasUpdate ? "发现新版本，点击查看" : "检查更新"}>
          <Badge dot={hasUpdate} size="small" offset={[-2, 4]}>
            <button
              className="icon-btn"
              onClick={() => setUpdateOpen(true)}
              aria-label="检查更新"
            >
              <Update size={15} />
            </button>
          </Badge>
        </Tooltip>

        <Tooltip title="设置">
          <button className="icon-btn" onClick={onOpenSettings} aria-label="设置">
            <Settings size={15} />
          </button>
        </Tooltip>

        {/* 窗口控制（自定义标题栏，与顶栏融为一体） */}
        <div className="window-controls">
          <button
            className="win-btn"
            onClick={() => appWindow.minimize()}
            aria-label="最小化"
          >
            <Minus size={14} />
          </button>
          <button
            className="win-btn"
            onClick={() => appWindow.toggleMaximize()}
            aria-label={isMaximized ? "还原" : "最大化"}
          >
            {isMaximized ? <Copy size={12} /> : <Square size={12} />}
          </button>
          <button
            className="win-btn close"
            onClick={() => appWindow.close()}
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>
      </div>

      {/* 关闭弹窗后静默复查一次，同步小红点状态（已更新/新版本上线） */}
      <UpdateModal
        open={updateOpen}
        onClose={() => {
          setUpdateOpen(false);
          void silentCheck();
        }}
      />
    </div>
  );
}
