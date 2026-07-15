import {useState} from "react";
import {App, Button, Empty, Popconfirm, Tooltip} from "antd";
import {CaretDown, CaretRight, FolderOpen, GitPull, GitPullRestart, Plus, Settings, Trash,} from "./Icons";
import {useStore} from "../store";
import * as api from "../api";
import ServiceCard from "./ServiceCard";
import ProjectConfigModal from "./ProjectConfigModal";
import type {Project, Service} from "../types";

interface Props {
  onAddProject: () => void;
  onAddService: () => void;
  onConfigService: (service: Service) => void;
}

// 提取到模块顶层：避免每次 ServiceList 渲染时创建新组件类型导致重挂载（丢失内部状态）
function ServiceRow({
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
}

export default function ServiceList({ onAddProject, onAddService, onConfigService }: Props) {
  const projects = useStore((s) => s.projects);
  const services = useStore((s) => s.services);
  const runtimes = useStore((s) => s.runtimes);
  const gitAvailable = useStore((s) => s.gitAvailable);
  const removeProject = useStore((s) => s.removeProject);
  const refreshServices = useStore((s) => s.refreshServices);
  const { message } = App.useApp();
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(() => {
    try {
      const saved = localStorage.getItem("jb_collapsed_groups");
      return saved ? JSON.parse(saved) : {};
    } catch {
      return {};
    }
  });
  const [configProject, setConfigProject] = useState<Project | null>(null);

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

  const handleDeleteProject = async (project: Project) => {
    try {
      await api.deleteProject(project.id);
      removeProject(project.id);
      message.success(`已删除项目 ${project.name}`);
    } catch (e: any) {
      message.error(`删除失败: ${e}`);
    }
  };

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
            <Tooltip title="项目环境配置（JDK / Maven）">
              <button
                className="icon-btn sm"
                onClick={() => setConfigProject(project)}
                aria-label="项目配置"
              >
                <Settings size={13} />
              </button>
            </Tooltip>
            {gitAvailable && project.git_available && (
              <>
                <Tooltip title={isBusy ? "有服务正在编译/启动中" : "Git 拉取"}>
                  <button
                    className="icon-btn sm accent"
                    disabled={isBusy}
                    onClick={() => handlePull(project, false)}
                    aria-label="Git 拉取"
                    style={isBusy ? { opacity: 0.4, cursor: "not-allowed" } : undefined}
                  >
                    <GitPull size={13} />
                  </button>
                </Tooltip>
                <Tooltip
                  title={isBusy ? "有服务正在编译/启动中" : "拉取并重启运行中的服务"}
                >
                  <button
                    className="icon-btn sm accent"
                    disabled={isBusy}
                    onClick={() => handlePull(project, true)}
                    aria-label="拉取并重启"
                    style={isBusy ? { opacity: 0.4, cursor: "not-allowed" } : undefined}
                  >
                    <GitPullRestart size={13} />
                  </button>
                </Tooltip>
              </>
            )}
            <Popconfirm
              title={`删除项目 "${project.name}"？`}
              description="将停止并删除该项目下所有服务。"
              onConfirm={() => handleDeleteProject(project)}
              okText="删除"
              cancelText="取消"
              okButtonProps={{ danger: true }}
            >
              <Tooltip title="删除项目">
                <button className="icon-btn sm danger" aria-label="删除项目">
                  <Trash size={13} />
                </button>
              </Tooltip>
            </Popconfirm>
          </div>
        </div>
        {!isCollapsed &&
          groupServices.map((s) => (
            <ServiceRow key={s.id} service={s} onConfig={onConfigService} />
          ))}
        {!isCollapsed && groupServices.length === 0 && (
          <div className="ungrouped-empty">// 无服务（点击项目头部可重新扫描）</div>
        )}
      </div>
    );
  };

  return (
    <div className="sidebar">
      <div className="sidebar-header">
        <span className="side-label">tree</span>
        <Button
          type="primary"
          size="small"
          icon={<Plus size={12} />}
          onClick={onAddProject}
          style={{ flex: 1 }}
        >
          添加项目
        </Button>
        <Button size="small" icon={<Plus size={12} />} onClick={onAddService}>
          服务
        </Button>
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
    </div>
  );
}
