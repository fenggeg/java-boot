import {memo, useState} from "react";
import {App, Button, Dropdown, Empty, Tooltip} from "antd";
import {
    CaretDown,
    CaretRight,
    ChevronLeft,
    File,
    FolderOpen,
    GitBranch,
    GitPull,
    GitPullRestart,
    Layers,
    More,
    Plus,
    Refresh,
    Settings,
    Trash,
} from "./Icons";
import {useStore} from "../store";
import * as api from "../api";
import ServiceCard from "./ServiceCard";
import ProjectConfigModal from "./ProjectConfigModal";
import AddProjectModal from "./AddProjectModal";
import type {Project, Service} from "../types";

interface Props {
  onAddProject: () => void;
  onAddService: () => void;
  onConfigService: (service: Service) => void;
  onOpenGit: (project: Project) => void;
  onOpenFiles: (project: Project) => void;
  collapsed: boolean;
  onToggleCollapse: () => void;
}

// 提取到模块顶层：避免每次 ServiceList 渲染时创建新组件类型导致重挂载（丢失内部状态）
// memo 化：切换选中态时只有 active 变化的卡片重渲染，其余跳过
const ServiceRow = memo(function ServiceRow({
  service,
  onConfig,
}: {
  service: Service;
  onConfig: (service: Service) => void;
}) {
  const selectedServiceId = useStore((s) => s.selectedServiceId);
  return (
    <ServiceCard
      service={service}
      active={selectedServiceId === service.id}
      onConfig={onConfig}
    />
  );
});

export default function ServiceList({ onAddProject, onAddService, onConfigService, onOpenGit, onOpenFiles, collapsed: sidebarCollapsed, onToggleCollapse: onToggleSidebar }: Props) {
  const projects = useStore((s) => s.projects);
  const services = useStore((s) => s.services);
  const runtimes = useStore((s) => s.runtimes);
  const gitAvailable = useStore((s) => s.gitAvailable);
  const removeProject = useStore((s) => s.removeProject);
  const refreshServices = useStore((s) => s.refreshServices);
  const { message, modal } = App.useApp();
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(() => {
    try {
      const saved = localStorage.getItem("jb_collapsed_groups");
      return saved ? JSON.parse(saved) : {};
    } catch {
      return {};
    }
  });
  const [configProject, setConfigProject] = useState<Project | null>(null);
  const [rescanProject, setRescanProject] = useState<Project | null>(null);

  const ungroupedServices = services.filter((s) => !s.project_id);

  const toggleCollapse = (id: string) => {
    setCollapsed((prev) => {
      const next = { ...prev, [id]: !prev[id] };
      try {
        localStorage.setItem("jb_collapsed_groups", JSON.stringify(next));
      } catch {
        /* ignore quota / serialization errors */
      }
      return next;
    });
  };

  const handlePull = async (project: Project, restart: boolean) => {
    try {
      const result = restart
        ? await api.gitPullAndRestart(project.id)
        : await api.gitPull(project.id);
      if (result.success) {
        if (result.up_to_date) {
          message.success(`${project.name}: 已是最新`);
        } else {
          message.success(`${project.name}: 拉取成功`);
        }
      } else {
        message.error(`${project.name}: 拉取失败 - ${result.message}`);
      }
      await refreshServices();
    } catch (e: any) {
      message.error(`拉取失败: ${e}`);
    }
  };

  const handleStartAll = async (project: Project) => {
    const groupServices = services.filter(
      (s) => s.project_id === project.id,
    );
    const stopped = groupServices.filter(
      (s) =>
        runtimes[s.id]?.status !== "running" &&
        runtimes[s.id]?.status !== "starting",
    );
    if (stopped.length === 0) {
      message.info("项目下所有服务已在运行中");
      return;
    }
    try {
      const ids = stopped.map((s) => s.id);
      const result = await api.startServicesBatch(ids);
      const okCount = result.succeeded.length;
      const failCount = result.failed.length;
      const skipCount = result.skipped.length;
      if (failCount === 0) {
        message.success(`已启动 ${okCount} 个服务${skipCount > 0 ? `（跳过 ${skipCount} 个已在运行的）` : ""}`);
      } else {
        const failNames = result.failed
          .map(([id, _]) => services.find((s) => s.id === id)?.name ?? id)
          .join("、");
        message.warning(
          `启动完成: ${okCount} 成功, ${failCount} 失败（${failNames}）${skipCount > 0 ? `, ${skipCount} 跳过` : ""}`,
        );
      }
      await refreshServices();
    } catch (e: any) {
      message.error(`启动失败: ${e}`);
    }
  };

  const handleDeleteProject = async (project: Project) => {
    try {
      await api.deleteProject(project.id);
      removeProject(project.id);
      message.success(`已删除项目 ${project.name}`);
    } catch (e: any) {
      message.error(`删除失败: ${e}`);
    }
  };

  // 顶部「添加」下拉菜单
  const addMenuItems = [
    { key: "project", label: "添加项目", icon: <Plus size={13} />, onClick: onAddProject },
    { key: "service", label: "添加服务", icon: <Plus size={13} />, onClick: onAddService },
  ];

  const renderProjectGroup = (project: Project) => {
    const groupServices = services.filter(
      (s) => s.project_id === project.id
    );
    const runningCount = groupServices.filter(
      (s) =>
        runtimes[s.id]?.status === "running" ||
        runtimes[s.id]?.status === "starting"
    ).length;
    const isCollapsed = collapsed[project.id];
    // 互斥检查：项目下是否有服务在编译/启动
    const isBusy = groupServices.some(
      (s) =>
        runtimes[s.id]?.status === "starting" ||
        runtimes[s.id]?.status === "recompiling" ||
        runtimes[s.id]?.status === "pulling"
    );

    // 项目级 More 下拉菜单（收纳低频操作）
    const moreMenuItems = [
      {
        key: "rescan",
        label: "重新扫描项目",
        icon: <Refresh size={13} />,
        onClick: () => setRescanProject(project),
      },
      {
        key: "config",
        label: "项目环境配置",
        icon: <Settings size={13} />,
        onClick: () => setConfigProject(project),
      },
      ...(gitAvailable && project.git_available
        ? [
            { type: "divider" as const },
            {
              key: "git",
              label: "Git 工作区",
              icon: <GitBranch size={13} />,
              onClick: () => onOpenGit(project),
            },
            {
              key: "pull",
              label: "Git 拉取",
              icon: <GitPull size={13} />,
              disabled: isBusy,
              onClick: () => handlePull(project, false),
            },
            {
              key: "pull-restart",
              label: "拉取并重启",
              icon: <GitPullRestart size={13} />,
              disabled: isBusy,
              onClick: () => handlePull(project, true),
            },
          ]
        : []),
      { type: "divider" as const },
      {
        key: "delete",
        label: "删除项目",
        danger: true,
        icon: <Trash size={13} />,
        onClick: () => {
          modal.confirm({
            title: `删除项目 "${project.name}"？`,
            content: "将停止并删除该项目下所有服务。",
            okText: "删除",
            cancelText: "取消",
            okButtonProps: { danger: true },
            onOk: () => handleDeleteProject(project),
          });
        },
      },
    ];

    return (
      <div key={project.id} className="project-group">
        <div
          className="project-group-header"
          onClick={() => toggleCollapse(project.id)}
        >
          <span className="caret">
            {isCollapsed ? <CaretRight size={10} /> : <CaretDown size={10} />}
          </span>
          <span className="group-icon">
            <FolderOpen size={14} />
          </span>
          <span className="group-name">{project.name}</span>
          <span className="project-group-count">
            {runningCount}/{groupServices.length}
          </span>
          <div
            className="project-group-actions"
            onClick={(e) => e.stopPropagation()}
          >
            {/* 高频：一键启动 */}
            <Tooltip title="一键启动项目下所有服务">
              <button
                className="icon-btn sm accent"
                disabled={isBusy}
                onClick={() => handleStartAll(project)}
                aria-label="全部启动"
                style={isBusy ? { opacity: 0.4, cursor: "not-allowed" } : undefined}
              >
                <Layers size={13} />
              </button>
            </Tooltip>
            {/* 文件浏览器：与启动按钮同一层级，直接可见 */}
            <Tooltip title="文件浏览器">
              <button
                className="icon-btn sm"
                onClick={() => onOpenFiles(project)}
                aria-label="文件浏览器"
              >
                <File size={13} />
              </button>
            </Tooltip>
            {/* 低频操作收纳到 More 下拉 */}
            <Dropdown menu={{ items: moreMenuItems }} trigger={["click"]} placement="bottomRight">
              <button className="icon-btn sm" aria-label="更多操作">
                <More size={14} />
              </button>
            </Dropdown>
          </div>
        </div>
        {!isCollapsed &&
          groupServices.map((s) => (
            <ServiceRow key={s.id} service={s} onConfig={onConfigService} />
          ))}
        {!isCollapsed && groupServices.length === 0 && (
          <div className="ungrouped-empty">
            暂无服务，点击「更多」→「重新扫描项目」发现模块
          </div>
        )}
      </div>
    );
  };

  if (sidebarCollapsed) {
    return (
      <div className="sidebar sidebar-collapsed">
        <Tooltip title="展开侧边栏" placement="right">
          <button
            className="icon-btn sidebar-expand-btn"
            onClick={onToggleSidebar}
            aria-label="展开侧边栏"
          >
            <ChevronLeft size={16} style={{ transform: "rotate(180deg)" }} />
          </button>
        </Tooltip>
      </div>
    );
  }

  return (
    <div className="sidebar">
      <div className="sidebar-header">
        <Dropdown menu={{ items: addMenuItems }} trigger={["click"]}>
          <Button
            type="primary"
            size="small"
            icon={<Plus size={12} />}
            style={{ flex: 1 }}
          >
            添加
          </Button>
        </Dropdown>
        <Tooltip title="收起侧边栏">
          <button
            className="icon-btn sm"
            onClick={onToggleSidebar}
            aria-label="收起侧边栏"
          >
            <ChevronLeft size={14} />
          </button>
        </Tooltip>
      </div>

      <div className="sidebar-list">
        {services.length === 0 && projects.length === 0 ? (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description="暂无服务，点击上方添加"
            style={{ marginTop: 60 }}
          />
        ) : (
          <>
            {projects.map(renderProjectGroup)}

            {ungroupedServices.length > 0 && (
              <div className="project-group">
                <div
                  className="project-group-header"
                  onClick={() => toggleCollapse("__ungrouped__")}
                >
                  <span className="caret">
                    {collapsed["__ungrouped__"] ? (
                      <CaretRight size={10} />
                    ) : (
                      <CaretDown size={10} />
                    )}
                  </span>
                  <span className="group-icon" style={{ color: "#86868b" }}>
                    <FolderOpen size={14} />
                  </span>
                  <span className="group-name">未分组</span>
                  <span className="project-group-count">
                    {ungroupedServices.length}
                  </span>
                </div>
                {!collapsed["__ungrouped__"] &&
                  ungroupedServices.map((s) => (
                    <ServiceRow key={s.id} service={s} onConfig={onConfigService} />
                  ))}
              </div>
            )}
          </>
        )}
      </div>
      <ProjectConfigModal
        project={configProject}
        onClose={() => setConfigProject(null)}
        onSaved={refreshServices}
      />
      <AddProjectModal
        open={!!rescanProject}
        project={rescanProject}
        onClose={() => setRescanProject(null)}
        onAdded={refreshServices}
      />
    </div>
  );
}
