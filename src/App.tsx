import {useEffect, useState} from "react";
import {Badge, Tabs} from "antd";
import {listen, type UnlistenFn,} from "@tauri-apps/api/event";
import {useStore} from "./store";
import type {LogLine, Project, Service, ServiceRuntime} from "./types";
import {STATUS_META} from "./types";
import TopBar from "./components/TopBar";
import ServiceList from "./components/ServiceList";
import LogViewer from "./components/LogViewer";
import GitPanel from "./components/GitPanel";
import AddProjectModal from "./components/AddProjectModal";
import AddServiceModal from "./components/AddServiceModal";
import ServiceConfigModal from "./components/ServiceConfigModal";
import SettingsDrawer from "./components/SettingsDrawer";
import {HeroLogo, Terminal} from "./components/Icons";

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
  // 右侧主视图：日志（默认）或某项目的 Git 面板
  const [view, setView] = useState<"logs" | "git">("logs");
  const [gitProjectId, setGitProjectId] = useState<string | null>(null);

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

  const handleAdded = async () => {
    await refreshServices();
  };

  // 打开某项目的 Git 面板
  const handleOpenGit = (project: Project) => {
    setGitProjectId(project.id);
    setView("git");
  };

  // 点击左侧服务（或切换日志 tab）时回到日志视图
  useEffect(() => {
    if (selectedServiceId) setView("logs");
  }, [selectedServiceId]);

  // 项目被删除时关闭对应的 Git 面板
  useEffect(() => {
    if (
      gitProjectId &&
      !projects.some((p) => p.id === gitProjectId)
    ) {
      setGitProjectId(null);
      setView("logs");
    }
  }, [projects, gitProjectId]);

  // 只渲染“已打开”的 tab（IDE-like），保持打开顺序
  const serviceMap = new Map(services.map((s) => [s.id, s]));
  const allTabs = openedTabs
    .map((id) => serviceMap.get(id))
    .filter((s): s is Service => !!s);

  const tabItems = allTabs.map((s) => {
    const rt = runtimes[s.id];
    const status = rt?.status ?? "stopped";
    const meta = STATUS_META[status];
    const logBuf = logs[s.id];
    const hasUnread = logBuf?.hasUnread ?? false;
    return {
      key: s.id,
      closable: true,
      label: (
        <span style={{ display: "flex", alignItems: "center", gap: 7 }}>
          <span
            className={`status-node ${meta.live ? "live" : ""}`}
            style={{ background: meta.dot, color: meta.dot }}
          />
          {s.name}
          {hasUnread && (
            <Badge
              color="#0071e3"
              style={{ width: 6, height: 6, minWidth: 6, boxShadow: "0 0 6px rgba(0,113,227,0.6)" }}
            />
          )}
        </span>
      ),
    };
  });

  return (
    <div className="app-layout">
      <TopBar onOpenSettings={() => setSettingsOpen(true)} />

      <div className="main-body">
        <ServiceList
          onAddProject={() => setAddProjectOpen(true)}
          onAddService={() => setAddServiceOpen(true)}
          onConfigService={setConfigService}
          onOpenGit={handleOpenGit}
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
