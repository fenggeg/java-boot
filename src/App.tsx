import {useCallback, useEffect, useMemo, useState} from "react";
import {App as AntApp, Dropdown, Tabs} from "antd";
import {listen, type UnlistenFn,} from "@tauri-apps/api/event";
import {useStore} from "./store";
import type {LogLine, Project, Service, ServiceRuntime} from "./types";
import {STATUS_META} from "./types";
import TopBar from "./components/TopBar";
import ServiceList from "./components/ServiceList";
import LogViewer from "./components/LogViewer";
import GitPanel from "./components/GitPanel";
import FilePanel from "./components/FilePanel";
import AddProjectModal from "./components/AddProjectModal";
import AddServiceModal from "./components/AddServiceModal";
import ServiceConfigModal from "./components/ServiceConfigModal";
import SettingsDrawer from "./components/SettingsDrawer";
import {HeroLogo, Terminal} from "./components/Icons";

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
  const logs = useStore((s) => s.logs);

  const [addProjectOpen, setAddProjectOpen] = useState(false);
  const [addServiceOpen, setAddServiceOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [configService, setConfigService] = useState<Service | null>(null);
  // 侧边栏折叠状态（持久化到 localStorage）
  const [sidebarCollapsed, setSidebarCollapsed] = useState<boolean>(() => {
    return localStorage.getItem("javaboot:sidebarCollapsed") === "1";
  });
  // 右侧主视图：日志（默认）、Git 面板或文件浏览器
  const [view, setView] = useState<"logs" | "git" | "files">("logs");
  const [gitProjectId, setGitProjectId] = useState<string | null>(null);
  const [fileProjectId, setFileProjectId] = useState<string | null>(null);
  // Tab 右键菜单上下文
  const [contextMenu, setContextMenu] = useState<{
    serviceId: string;
    x: number;
    y: number;
  } | null>(null);
  const { message } = AntApp.useApp();

  // 初始化 + 事件监听
  useEffect(() => {
    init();

    let unlistenStatus: UnlistenFn | undefined;
    let unlistenLog: UnlistenFn | undefined;

    (async () => {
      unlistenStatus = await listen<ServiceRuntime>(
        "service://status",
        (e) => {
          setRuntime(e.payload);
        }
      );
      unlistenLog = await listen<LogLine>("service://log", (e) => {
        appendLog(e.payload);
      });
    })();

    return () => {
      unlistenStatus?.();
      unlistenLog?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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

  // 打开某项目的 Git 面板
  const handleOpenGit = useCallback((project: Project) => {
    setGitProjectId(project.id);
    setView("git");
  }, []);

  // 打开某项目的文件浏览器
  const handleOpenFiles = useCallback((project: Project) => {
    setFileProjectId(project.id);
    setView("files");
  }, []);

  // 点击左侧服务（或切换日志 tab）时回到日志视图
  useEffect(() => {
    if (selectedServiceId) setView("logs");
  }, [selectedServiceId]);

  // 项目被删除时关闭对应的 Git 面板 / 文件浏览器
  useEffect(() => {
    if (gitProjectId && !projects.some((p) => p.id === gitProjectId)) {
      setGitProjectId(null);
      setView("logs");
    }
    if (fileProjectId && !projects.some((p) => p.id === fileProjectId)) {
      setFileProjectId(null);
      setView("logs");
    }
  }, [projects, gitProjectId, fileProjectId]);

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

  // 只渲染“已打开”的 tab（IDE-like），保持打开顺序
  const tabItems = useMemo(() => {
    const serviceMap = new Map(services.map((s) => [s.id, s]));
    const allTabs = openedTabs
      .map((id) => serviceMap.get(id))
      .filter((s): s is Service => !!s);

    return allTabs.map((s) => {
      const rt = runtimes[s.id];
      const status = rt?.status ?? "stopped";
      const meta = STATUS_META[status];
      const logBuf = logs[s.id];
      const hasUnread = logBuf?.hasUnread ?? false;
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
  }, [services, openedTabs, runtimes, logs, handleContextMenu]);

  return (
    <div className="app-layout">
      <TopBar onOpenSettings={() => setSettingsOpen(true)} />

      <div className="main-body">
        <ServiceList
          onAddProject={() => setAddProjectOpen(true)}
          onAddService={() => setAddServiceOpen(true)}
          onConfigService={setConfigService}
          onOpenGit={handleOpenGit}
          onOpenFiles={handleOpenFiles}
          collapsed={sidebarCollapsed}
          onToggleCollapse={toggleSidebar}
        />

        <div className="log-panel">
          {view === "git" && gitProjectId ? (
            (() => {
              const project = projects.find((p) => p.id === gitProjectId);
              return project ? (
                <GitPanel
                  project={project}
                  onClose={() => setView("logs")}
                />
              ) : null;
            })()
          ) : view === "files" && fileProjectId ? (
            (() => {
              const project = projects.find((p) => p.id === fileProjectId);
              return project ? (
                <FilePanel
                  project={project}
                  onClose={() => setView("logs")}
                />
              ) : null;
            })()
          ) : services.length === 0 ? (
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
              </div>
              <LogViewer serviceId={selectedServiceId} />
            </>
          )}
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
