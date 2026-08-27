import {memo, useState} from "react";
import {App, Dropdown, Switch, Tooltip} from "antd";
import {Broom, Code, More, Play, Restart, Settings, Stop, Warning,} from "./Icons";
import {useStore} from "../store";
import type {Service} from "../types";
import {STATUS_META} from "../types";
import * as api from "../api";

interface Props {
  service: Service;
  active: boolean;
  onConfig: (service: Service) => void;
}

// 噪声端口（JMX/DevTools/H2 等）由后端在 runtime.ports / runtime.service_ports
// 返回前统一过滤，前端无需维护重复列表，避免前后端漂移。

function ServiceCardInner({ service, active, onConfig }: Props) {
  const runtime = useStore((s) => s.runtimes[service.id]);
  const selectService = useStore((s) => s.selectService);
  const refreshServices = useStore((s) => s.refreshServices);
  const removeService = useStore((s) => s.removeService);
  const { message, modal } = App.useApp();

  const status = runtime?.status ?? "stopped";
  const meta = STATUS_META[status];
  const isRunning =
    status === "running" || status === "starting" || status === "recompiling";
  // 操作进行中标志：执行启动/重启/编译期间禁用所有操作按钮，防止并发触发
  // 导致 handles map 上的 placeholder 被误判为堆叠残留而 kill 掉刚启动的进程。
  const [busy, setBusy] = useState(false);
  const logBuf = useStore((s) => s.logs[service.id]);
  const hasUnread = logBuf?.hasUnread ?? false;

  const handleStart = async () => {
    if (busy) return;
    setBusy(true);
    try {
      // 统一走带依赖编排的启动流程：后端根据实际依赖决定是否编排，
      // 避免前端预判依赖（getServiceDependencies）与实际启动之间的 TOCTOU 竞态，
      // 同时省掉一次 IPC 往返。无依赖时等价于普通启动。
      await api.startServiceWithDependencies(service.id);
      // 启动是异步的，真实结果通过 service://status 事件通知
    } catch (e) {
      message.error(`启动失败: ${api.toErrMsg(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleStop = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await api.stopService(service.id);
    } catch (e) {
      message.error(`停止失败: ${api.toErrMsg(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleRestart = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await api.restartService(service.id);
      // 重启是异步的，真实结果通过事件通知
    } catch (e) {
      message.error(`重启失败: ${api.toErrMsg(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleCompile = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await api.compileAndStart(service.id);
      // 编译并启动是异步的
    } catch (e) {
      message.error(`编译失败: ${api.toErrMsg(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleRecompile = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await api.recompileAndStart(service.id);
      // 重新编译并启动是异步的
    } catch (e) {
      message.error(`重新编译失败: ${api.toErrMsg(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const handleClean = async () => {
    try {
      await api.cleanService(service.id);
      message.success(`${service.name}: 清理完成`);
    } catch (e) {
      message.error(`清理失败: ${api.toErrMsg(e)}`);
    }
  };

  const handleDelete = async () => {
    try {
      await api.deleteService(service.id);
      removeService(service.id);
      message.success("已删除服务");
    } catch (e) {
      message.error(`删除失败: ${api.toErrMsg(e)}`);
    }
  };

  const handlePortClick = (port: number) => {
    api.openInBrowser(port).catch((e) => message.error(`打开失败: ${api.toErrMsg(e)}`));
  };

  const handleToggleAutoRestart = async (enabled: boolean) => {
    try {
      await api.toggleAutoRestart(service.id, enabled);
      await refreshServices();
    } catch (e) {
      message.error(`操作失败: ${api.toErrMsg(e)}`);
    }
  };

  const menuItems = [
    {
      key: "config",
      label: "服务配置",
      icon: <Settings size={13} />,
      onClick: () => onConfig(service),
    },
    {
      key: "compile",
      label: "编译并启动",
      icon: <Code size={13} />,
      onClick: handleCompile,
    },
    {
      key: "recompile",
      label: (
        <Tooltip
          title="强制 mvn clean compile 后启动；编译失败则服务停止（与「编译并启动」不同，后者失败时保留旧进程）"
          placement="right"
        >
          <span>重新编译并启动</span>
        </Tooltip>
      ),
      icon: <Code size={13} />,
      onClick: handleRecompile,
    },
    {
      key: "clean",
      label: "清理编译产物",
      icon: <Broom size={13} />,
      disabled: isRunning,
      onClick: handleClean,
    },
    { type: "divider" as const },
    {
      key: "delete",
      label: "删除服务",
      danger: true,
      onClick: () => {
        modal.confirm({
          title: "确认删除该服务？",
          content: "删除后不可恢复，若服务正在运行将先停止。",
          okText: "删除",
          cancelText: "取消",
          okButtonProps: { danger: true },
          onOk: handleDelete,
        });
      },
    },
  ];

  return (
    <div
      className={`service-card ${active ? "active" : ""} ${
        runtime?.port_conflict ? "conflict" : ""
      }`}
      onClick={() => selectService(service.id)}
    >
      {/* 状态点 */}
      <span
        className={`status-node ${meta.live ? "live" : ""}`}
        style={{ background: meta.dot, color: meta.dot }}
      />

      {hasUnread && <span className="unread-badge" />}

      <div className="service-card-info">
        <span className="service-card-name">{service.name}</span>
        <div className="service-card-meta">
          <span className="meta-status" style={{ color: meta.dot }}>
            {meta.label}
          </span>
          {runtime?.pid && <span className="meta-pid">pid:{runtime.pid}</span>}
          {runtime?.cpu_usage != null && status === "running" && (
            <span>cpu:{runtime.cpu_usage.toFixed(1)}%</span>
          )}
          {runtime?.memory_mb != null && status === "running" && (
            <span>
              {runtime.memory_mb < 1024
                ? `${runtime.memory_mb.toFixed(0)}m`
                : `${(runtime.memory_mb / 1024).toFixed(1)}g`}
            </span>
          )}
          {(() => {
            // 优先展示从启动日志解析出的 HTTP 服务端口；
            // 为空时回退到 runtime.ports（后端已过滤噪声端口）
            const ports =
              runtime?.service_ports && runtime.service_ports.length > 0
                ? runtime.service_ports
                : runtime?.ports ?? [];
            if (ports.length === 0) return null;
            return (
              <div className="service-card-ports">
                {ports.map((p) => (
                  <Tooltip key={p} title={`在浏览器中打开 :${p}`}>
                    <span
                      className={`port-tag ${runtime?.port_conflict ? "conflict" : ""}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        handlePortClick(p);
                      }}
                    >
                      :{p}
                    </span>
                  </Tooltip>
                ))}
              </div>
            );
          })()}
          {runtime?.port_conflict && (
            <Tooltip
              title={`端口冲突: ${runtime.conflict_with.join(", ")}`}
            >
              <span style={{ display: "inline-flex", color: "#ff3b30" }}>
                <Warning size={12} />
              </span>
            </Tooltip>
          )}
        </div>
      </div>

      <div className="service-card-actions" onClick={(e) => e.stopPropagation()}>
        <Tooltip title={service.auto_restart ? "关闭自动重启" : "开启自动重启"}>
          <Switch
            size="small"
            checked={service.auto_restart}
            onChange={handleToggleAutoRestart}
          />
        </Tooltip>
        {!isRunning ? (
          <Tooltip title="启动">
            <button
              className="icon-btn sm"
              onClick={handleStart}
              disabled={busy}
              aria-label="启动"
              style={{ color: "#34c759" }}
            >
              <Play size={13} />
            </button>
          </Tooltip>
        ) : (
          <Tooltip title="停止">
            <button
              className="icon-btn sm danger"
              onClick={handleStop}
              disabled={busy}
              aria-label="停止"
              style={{ color: "#ff3b30" }}
            >
              <Stop size={12} />
            </button>
          </Tooltip>
        )}
        <Tooltip title="重启">
          <button
            className="icon-btn sm"
            onClick={handleRestart}
            disabled={busy || status === "starting" || status === "recompiling"}
            aria-label="重启"
            style={{ color: "#0071e3" }}
          >
            <Restart size={13} />
          </button>
        </Tooltip>
        <Dropdown menu={{ items: menuItems }} trigger={["click"]}>
          <button className="icon-btn sm" aria-label="更多">
            <More size={14} />
          </button>
        </Dropdown>
      </div>
    </div>
  );
}

export default memo(ServiceCardInner);
