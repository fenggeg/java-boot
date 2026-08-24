import {useMemo, useState} from "react";
import {App, Button, Popconfirm, Tooltip} from "antd";
import {GitPull, Logo, Moon, Settings, Stop, Sun} from "./Icons";
import {useStore} from "../store";
import * as api from "../api";
import {STATUS_META} from "../types";
import {useThemeStore} from "../theme";

interface Props {
  onOpenSettings: () => void;
}

export default function TopBar({ onOpenSettings }: Props) {
  const services = useStore((s) => s.services);
  const runtimes = useStore((s) => s.runtimes);
  const gitAvailable = useStore((s) => s.gitAvailable);
  const refreshServices = useStore((s) => s.refreshServices);
  const { message } = App.useApp();
  const themeMode = useThemeStore((s) => s.mode);
  const toggleTheme = useThemeStore((s) => s.toggle);
  const [hoverTheme, setHoverTheme] = useState(false);

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
    } catch (e: any) {
      message.error(`停止失败: ${e}`);
    }
  };

  return (
    <div className="topbar">
      <div className="topbar-brand">
        <span className="brand-mark">
          <Logo size={28} />
        </span>
        <div className="brand-word">
          <span className="brand-name">
            java<span className="accent">boot</span>
          </span>
          <span className="brand-sub">launcher · v0.1</span>
        </div>
      </div>

      <div className="topbar-stats">
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
        {!gitAvailable && (
          <Tooltip title="未检测到 Git，Git 拉取功能已禁用。请安装 Git 并配置到 PATH。">
            <span
              style={{
                display: "inline-flex",
                alignItems: "center",
                gap: 5,
                marginLeft: 4,
                paddingLeft: 14,
                borderLeft: "1px solid var(--border)",
                color: "#ff9500",
                fontSize: 12,
                fontWeight: 500,
              }}
            >
              <GitPull size={14} />
              Git 不可用
            </span>
          </Tooltip>
        )}
      </div>

      <div className="topbar-actions">
        {running > 0 && (
          <Popconfirm
            title="停止所有运行中的服务？"
            onConfirm={handleStopAll}
            okText="停止全部"
            cancelText="取消"
            okButtonProps={{ danger: true }}
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

        <Tooltip title="设置">
          <button className="icon-btn" onClick={onOpenSettings} aria-label="设置">
            <Settings size={15} />
          </button>
        </Tooltip>
      </div>
    </div>
  );
}
