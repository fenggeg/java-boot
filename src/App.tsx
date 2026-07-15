import { useEffect, useState } from "react";
import { Tabs, Badge, Empty } from "antd";
import {
  listen,
  type UnlistenFn,
} from "@tauri-apps/api/event";
import { useStore } from "./store";
import type { ServiceRuntime, LogLine, Service } from "./types";
import { STATUS_META } from "./types";
import TopBar from "./components/TopBar";
import ServiceList from "./components/ServiceList";
import LogViewer from "./components/LogViewer";
import AddProjectModal from "./components/AddProjectModal";
import AddServiceModal from "./components/AddServiceModal";
import ServiceConfigModal from "./components/ServiceConfigModal";
import SettingsDrawer from "./components/SettingsDrawer";

export default function App() {
  const init = useStore((s) => s.init);
  const refreshServices = useStore((s) => s.refreshServices);
  const setRuntime = useStore((s) => s.setRuntime);
  const appendLog = useStore((s) => s.appendLog);
  const services = useStore((s) => s.services);
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
              color="#22d3ee"
              style={{ width: 6, height: 6, minWidth: 6, boxShadow: "0 0 6px #22d3ee" }}
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
        />

        <div className="log-panel">
          {services.length === 0 ? (
            <div className="hero-empty">
              <pre className="hero-ascii">{`  ┌─────────────────────┐
  │  ▶  J A V A B O O T  │
  └─────────────────────┘`}</pre>
              <div className="hero-title">
                SpringBoot Launcher<span className="cursor" />
              </div>
              <div className="hero-sub">
                轻量本地服务编排 · 点击左侧 <span className="accent">+ 添加项目</span> 开始
              </div>
            </div>
          ) : openedTabs.length === 0 ? (
            <div className="hero-empty">
              <div className="hero-sub">
                ← 从左侧服务列表点击一个服务，在此查看日志
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
