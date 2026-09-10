import {lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState} from "react";
import {App as AntApp, Dropdown, Tabs, Alert, Button, Spin} from "antd";
import {listen, type UnlistenFn,} from "@tauri-apps/api/event";
import {useShallow} from "zustand/react/shallow";
import {useStore} from "./store";
import type {LogLine, Project, Service, ServiceRuntime} from "./types";
import {STATUS_META} from "./types";
import TopBar from "./components/TopBar";
import ServiceList from "./components/ServiceList";
import LogViewer from "./components/LogViewer";
// 文件面板懒加载：其依赖链包含 monaco-editor（3MB+），静态引入会在启动时
// 解析全部 Monaco 导致首帧白屏久；改为首次打开文件视图时才加载
const FilePanel = lazy(() => import("./components/FilePanel"));
import AddProjectModal from "./components/AddProjectModal";
import AddServiceModal from "./components/AddServiceModal";
import ServiceConfigModal from "./components/ServiceConfigModal";
import SettingsDrawer from "./components/SettingsDrawer";
import {HeroLogo, Terminal, ChevronLeft, Code, X} from "./components/Icons";

type ContextMenuAction =
  | "close"
  | "closeOthers"
  | "closeAll"
  | "copyName";

export default function App() {
  const init = useStore((s) => s.init);
  const refreshServices = useStore((s) => s.refreshServices);
  const setRuntime = useStore((s) => s.setRuntime);
  const appendLog = useStore((s) => s.appendLog);
  const services = useStore((s) => s.services);
  const projects = useStore((s) => s.projects);
  const runtimes = useStore((s) => s.runtimes);
  const selectedServiceId = useStore((s) => s.selectedServiceId);
  const selectService = useStore((s) => s.selectService);
  const closeTab = useStore((s) => s.closeTab);
  const openedTabs = useStore((s) => s.openedTabs);
  const initError = useStore((s) => s.initError);
  // ---- P3 daemon 监控闭环 ----
  const setDaemonConnected = useStore((s) => s.setDaemonConnected);
  const setDaemonProcesses = useStore((s) => s.setDaemonProcesses);
  const updateDaemonMetrics = useStore((s) => s.updateDaemonMetrics);
  const refreshDaemon = useStore((s) => s.refreshDaemon);

  // 精确订阅 hasUnread：只在未读状态变化时触发重渲染，日志行变化不触发。
  // 避免任意服务一条日志导致所有 Tab 标签重算。
  // useShallow 对返回对象做浅比较，防止选择器每次返回新对象引发无限重渲染（React #185）。
  const unreadMap = useStore(
    useShallow((s) => {
      const m: Record<string, boolean> = {};
      for (const id of s.openedTabs) {
        m[id] = s.logs[id]?.hasUnread ?? false;
      }
      return m;
    })
  );

  const [addProjectOpen, setAddProjectOpen] = useState(false);
  const [addServiceOpen, setAddServiceOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [configService, setConfigService] = useState<Service | null>(null);
  // 侧边栏折叠状态（持久化到 localStorage）
  const [sidebarCollapsed, setSidebarCollapsed] = useState<boolean>(() => {
    return localStorage.getItem("javaboot:sidebarCollapsed") === "1";
  });
  // 分屏：日志常驻左侧，文件面板为右侧 Dock（可关、可拖宽、日志可收起）
  const [fileProjectId, setFileProjectId] = useState<string | null>(null);
  const [fileDockOpen, setFileDockOpen] = useState(false);
  const [dockWidth, setDockWidth] = useState<number>(() => {
    const saved = localStorage.getItem("javaboot:dockWidth");
    return saved ? parseInt(saved, 10) || 520 : 520;
  });
  /** 文件 Dock 打开时，是否收起日志面板（腾出主区给文件树/编辑器） */
  const [logCollapsed, setLogCollapsed] = useState<boolean>(() => {
    return localStorage.getItem("javaboot:logCollapsed") === "1";
  });
  const dockDraggingRef = useRef(false);
  const [dockDragging, setDockDragging] = useState(false);
  const dockAreaRef = useRef<HTMLDivElement>(null);
  const splitRef = useRef<HTMLDivElement>(null);
  // Tab 右键菜单上下文
  const [contextMenu, setContextMenu] = useState<{
    serviceId: string;
    x: number;
    y: number;
  } | null>(null);
  const { message } = AntApp.useApp();

  // Dock 拖拽调宽：以 content-split 右缘为基准算宽度（Dock 固定在右侧）
  // 旧算法用 e.clientX - dock.left，在 flex 右贴布局下结果为负/跳变，导致拖不动
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!dockDraggingRef.current || !splitRef.current) return;
      const split = splitRef.current.getBoundingClientRect();
      // 光标到分屏右缘的距离 = dock 目标宽度
      const raw = split.right - e.clientX;
      // 拖到很窄时自动收起日志侧并让 dock 充满；拖回则恢复
      if (raw < 80 && !logCollapsed) {
        setLogCollapsed(true);
        localStorage.setItem("javaboot:logCollapsed", "1");
        return;
      }
      if (raw > 140 && logCollapsed) {
        setLogCollapsed(false);
        localStorage.setItem("javaboot:logCollapsed", "0");
      }
      const next = Math.max(360, Math.min(Math.round(raw), split.width - 80));
      setDockWidth(next);
    };
    const onUp = () => {
      if (!dockDraggingRef.current) return;
      dockDraggingRef.current = false;
      setDockDragging(false);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      setDockWidth((w) => {
        localStorage.setItem("javaboot:dockWidth", String(w));
        return w;
      });
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [logCollapsed]);

  // 初始化 + 事件监听
  useEffect(() => {
    init();

    let unlistenStatus: UnlistenFn | undefined;
    let unlistenLog: UnlistenFn | undefined;
    // 防止 listen resolve 前 cleanup 已执行导致监听泄漏
    const disposedRef = { current: false };

    (async () => {
      unlistenStatus = await listen<ServiceRuntime>(
        "service://status",
        (e) => {
          setRuntime(e.payload);
        }
      );
      if (disposedRef.current) {
        unlistenStatus();
        unlistenStatus = undefined;
        return;
      }
      unlistenLog = await listen<LogLine>("service://log", (e) => {
        appendLog(e.payload);
      });
      if (disposedRef.current) {
        unlistenLog();
        unlistenLog = undefined;
        return;
      }
    })();

    // ---- P3 daemon 事件订阅：连接状态 / 实时监控指标 ----
    const unlisteners: UnlistenFn[] = [];
    let disposed2 = false;
    (async () => {
      const on = <T,>(name: string, fn: (payload: T) => void) => {
        listen<T>(name, (e) => {
          if (!disposed2) fn(e.payload);
        }).then((u) => {
          if (disposed2) u();
          else unlisteners.push(u);
        });
      };
      on("daemon-connected", () =>
        setDaemonConnected(true),
      );
      on("daemon-disconnected", () => {
        setDaemonConnected(false);
        setDaemonProcesses([]);
      });
      on("daemon-proc-metrics", (m: { run_id: number; cpu_usage: number | null; memory_mb: number | null }) =>
        updateDaemonMetrics(m.run_id, m.cpu_usage, m.memory_mb),
      );
      // 首次拉一次对账，让 daemon 进程列表与连接状态就绪
      void refreshDaemon();
    })();

    return () => {
      disposedRef.current = true;
      disposed2 = true;
      for (const u of unlisteners) u();
      unlistenStatus?.();
      unlistenLog?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ---- P3：周期对账 daemon 进程列表（加入/退出），指标由 proc-metrics 事件实时更新 ----
  useEffect(() => {
    const timer = setInterval(() => {
      void refreshDaemon();
    }, 4000);
    return () => clearInterval(timer);
  }, [refreshDaemon]);

  const handleAdded = useCallback(async () => {
    await refreshServices();
  }, [refreshServices]);

  const toggleSidebar = useCallback(() => {
    setSidebarCollapsed((prev) => {
      const next = !prev;
      localStorage.setItem("javaboot:sidebarCollapsed", next ? "1" : "0");
      return next;
    });
  }, []);

  // 打开某项目的文件浏览器（右侧 Dock，与日志并存）
  const handleOpenFiles = useCallback((project: Project) => {
    setFileProjectId(project.id);
    setFileDockOpen(true);
  }, []);

  // 收起编辑器 Dock（保留日志视图；若日志已收起则一并展开）
  const closeFileDock = useCallback(() => {
    setFileDockOpen(false);
    setLogCollapsed(false);
    try {
      localStorage.setItem("javaboot:logCollapsed", "0");
    } catch {
      /* ignore */
    }
  }, []);

  /** 日志页「打开/收起编辑器」：优先用当前 Dock 项目，其次当前服务所属项目，再取第一个项目 */
  const toggleEditorDock = useCallback(() => {
    if (fileDockOpen) {
      closeFileDock();
      return;
    }
    const svc = services.find((s) => s.id === selectedServiceId);
    const project =
      projects.find((p) => p.id === fileProjectId) ??
      projects.find((p) => p.id === svc?.project_id) ??
      projects[0];
    if (!project) {
      message.info("请先添加项目后再打开编辑器");
      return;
    }
    setFileProjectId(project.id);
    setFileDockOpen(true);
  }, [fileDockOpen, closeFileDock, fileProjectId, selectedServiceId, services, projects, message]);

  const toggleLogCollapsed = useCallback(() => {
    setLogCollapsed((prev) => {
      const next = !prev;
      try {
        localStorage.setItem("javaboot:logCollapsed", next ? "1" : "0");
      } catch {
        /* ignore */
      }
      return next;
    });
  }, []);

  // 项目被删除时关闭对应的文件 Dock
  useEffect(() => {
    if (fileProjectId && !projects.some((p) => p.id === fileProjectId)) {
      setFileProjectId(null);
      setFileDockOpen(false);
    }
  }, [projects, fileProjectId]);

  // Dock 左缘拖拽开始
  const startDockDrag = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dockDraggingRef.current = true;
    setDockDragging(true);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, []);

  // Tab 右键菜单处理
  const handleContextMenu = useCallback((e: React.MouseEvent, serviceId: string) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ serviceId, x: e.clientX, y: e.clientY });
  }, []);

  const closeContextMenu = useCallback(() => setContextMenu(null), []);

  const handleContextAction = useCallback((action: ContextMenuAction, serviceId: string) => {
    switch (action) {
      case "close":
        closeTab(serviceId);
        break;
      case "closeOthers": {
        const others = openedTabs.filter((id) => id !== serviceId);
        others.forEach((id) => closeTab(id));
        break;
      }
      case "closeAll":
        openedTabs.forEach((id) => closeTab(id));
        break;
      case "copyName": {
        const svc = services.find((s) => s.id === serviceId);
        if (svc) {
          navigator.clipboard.writeText(svc.name).then(() => {
            message.success(`已复制: ${svc.name}`);
          }).catch(() => {});
        }
        break;
      }
    }
    closeContextMenu();
  }, [closeTab, openedTabs, services, message, closeContextMenu]);

  // 只渲染"已打开"的 tab（IDE-like），保持打开顺序
  const tabItems = useMemo(() => {
    const serviceMap = new Map(services.map((s) => [s.id, s]));
    const allTabs = openedTabs
      .map((id) => serviceMap.get(id))
      .filter((s): s is Service => !!s);

    return allTabs.map((s) => {
      const rt = runtimes[s.id];
      const status = rt?.status ?? "stopped";
      const meta = STATUS_META[status];
      const hasUnread = unreadMap[s.id] ?? false;
      return {
        key: s.id,
        closable: true,
        label: (
          <span
            style={{ display: "flex", alignItems: "center", gap: 7 }}
            onContextMenu={(e) => handleContextMenu(e, s.id)}
          >
            <span
              className={`status-node ${meta.live ? "live" : ""}`}
              style={{ background: meta.dot, color: meta.dot }}
            />
            {s.name}
            {hasUnread && <span className="unread-badge" />}
          </span>
        ),
      };
    });
  }, [services, openedTabs, runtimes, unreadMap, handleContextMenu]);

  return (
    <div className="app-layout">
      {initError && (
        <Alert
          type="error"
          showIcon
          banner
          message="初始化失败"
          description={initError}
          action={
            <Button size="small" onClick={() => init()}>
              重试
            </Button>
          }
        />
      )}
      <TopBar onOpenSettings={() => setSettingsOpen(true)} />

      <div className="main-body">
        <ServiceList
          onAddProject={() => setAddProjectOpen(true)}
          onAddService={() => setAddServiceOpen(true)}
          onConfigService={setConfigService}
          onOpenFiles={handleOpenFiles}
          collapsed={sidebarCollapsed}
          onToggleCollapse={toggleSidebar}
        />

        {/* 分屏主区：日志左（可收起），文件 Dock 右（可拖宽） */}
        <div
          ref={splitRef}
          className={`content-split ${dockDragging ? "is-dragging" : ""} ${logCollapsed && fileDockOpen ? "log-collapsed" : ""}`}
        >
          {/* 日志侧：Dock 打开且收起时隐藏，仅留窄条恢复入口 */}
          {!(fileDockOpen && logCollapsed) ? (
            <div className="log-panel">
              {services.length === 0 ? (
                <div className="hero-empty">
                  <div className="hero-mark">
                    <HeroLogo size={88} />
                  </div>
                  <div className="hero-title">JavaBoot Launcher</div>
                  <div className="hero-sub">
                    轻量本地 Spring Boot 服务编排。点击左侧
                    <span className="accent"> 添加项目</span> 开始
                  </div>
                </div>
              ) : openedTabs.length === 0 ? (
                <div className="hero-empty">
                  <div className="hero-mark subtle">
                    <Terminal size={56} />
                  </div>
                  <div className="hero-sub">
                    从左侧服务列表选择一个服务，在此查看实时日志
                  </div>
                </div>
              ) : (
                <>
                  <div className="log-tabs">
                    <Tabs
                      size="small"
                      type="editable-card"
                      hideAdd
                      activeKey={selectedServiceId ?? undefined}
                      onChange={(key) => selectService(key)}
                      onEdit={(key, action) => {
                        if (action === "remove" && typeof key === "string") {
                          closeTab(key);
                        }
                      }}
                      items={tabItems}
                      tabBarStyle={{ margin: 0, padding: "4px 8px 0" }}
                    />
                    {projects.length > 0 && (
                      <div className="log-tabs-actions">
                        <button
                          className={`log-collapse-btn ${fileDockOpen ? "accent" : ""}`}
                          onClick={toggleEditorDock}
                          title={fileDockOpen ? "收起编辑器" : "打开编辑器"}
                          aria-label={fileDockOpen ? "收起编辑器" : "打开编辑器"}
                          aria-pressed={fileDockOpen}
                        >
                          <Code size={14} />
                        </button>
                        {fileDockOpen && (
                          <button
                            className="log-collapse-btn"
                            onClick={toggleLogCollapsed}
                            title="收起日志（双击分隔条）"
                            aria-label="收起日志"
                          >
                            <ChevronLeft size={14} />
                          </button>
                        )}
                      </div>
                    )}
                  </div>
                  <LogViewer serviceId={selectedServiceId} />
                </>
              )}
            </div>
          ) : (
            <button
              className="log-expand-strip"
              onClick={toggleLogCollapsed}
              title="展开日志"
              aria-label="展开日志"
            >
              <ChevronLeft size={14} style={{ transform: "rotate(180deg)" }} />
              <span>日志</span>
            </button>
          )}

          {(() => {
            const fileProject = projects.find((p) => p.id === fileProjectId);
            if (!fileDockOpen || !fileProject) return null;
            return (
              <>
                <div
                  className="dock-resizer"
                  onMouseDown={startDockDrag}
                  onDoubleClick={toggleLogCollapsed}
                  title="拖动调整宽度；双击收起/展开日志"
                />
                <div
                  className="file-dock"
                  ref={dockAreaRef}
                  style={{ width: dockWidth }}
                >
                  <div className="dock-header">
                    <span className="dock-header-title">
                      <Code size={13} />
                      编辑器
                    </span>
                    <button
                      className="icon-btn sm"
                      onClick={closeFileDock}
                      title="收起编辑器"
                      aria-label="收起编辑器"
                    >
                      <X size={13} />
                    </button>
                  </div>
                  <div className="dock-body">
                    <Suspense
                      fallback={
                        <div style={{ padding: 60, textAlign: "center" }}>
                          <Spin />
                        </div>
                      }
                    >
                      <FilePanel project={fileProject} visible={fileDockOpen} />
                    </Suspense>
                  </div>
                </div>
              </>
            );
          })()}
        </div>
      </div>

      {/* Tab 右键菜单 */}
      {contextMenu && (
        <Dropdown
          open={true}
          trigger={["contextMenu"]}
          onOpenChange={(open) => { if (!open) closeContextMenu(); }}
          menu={{
            items: [
              { key: "close", label: "关闭", onClick: () => handleContextAction("close", contextMenu.serviceId) },
              { key: "closeOthers", label: "关闭其他", disabled: openedTabs.length <= 1, onClick: () => handleContextAction("closeOthers", contextMenu.serviceId) },
              { key: "closeAll", label: "关闭全部", onClick: () => handleContextAction("closeAll", contextMenu.serviceId) },
              { type: "divider" as const },
              { key: "copyName", label: "复制服务名", onClick: () => handleContextAction("copyName", contextMenu.serviceId) },
            ],
          }}
        >
          <div
            style={{
              position: "fixed",
              left: contextMenu.x,
              top: contextMenu.y,
              // 锚点必须非 0 尺寸：0x0 的 fixed 元素会被 rc-trigger 判定为
              // 不可见（offsetParent 为 null 且宽高为 0），导致弹层永不对齐、菜单无法显示
              width: 1,
              height: 1,
              opacity: 0,
              pointerEvents: "none",
            }}
          />
        </Dropdown>
      )}

      <AddProjectModal
        open={addProjectOpen}
        onClose={() => setAddProjectOpen(false)}
        onAdded={handleAdded}
      />
      <AddServiceModal
        open={addServiceOpen}
        onClose={() => setAddServiceOpen(false)}
        onAdded={handleAdded}
      />
      <ServiceConfigModal
        service={configService}
        onClose={() => setConfigService(null)}
        onSaved={handleAdded}
      />
      <SettingsDrawer
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
      />
    </div>
  );
}
