import {invoke} from "@tauri-apps/api/core";
import type {
    AppConfig,
    BatchStartResult,
    FileContent,
    FileEntry,
    GitCommitInfo,
    GitStatus,
    JdkInfo,
    MavenInfo,
    Project,
    PullResult,
    ScannedModule,
    Service,
    ServiceRuntime,
} from "./types";

// ============================ Project ============================

export const listProjects = () => invoke<Project[]>("list_projects");
export const listServices = () => invoke<Service[]>("list_services");
export const scanProject = (path: string) =>
  invoke<ScannedModule[]>("scan_project", { path });
export const addProject = (path: string, selectedModules: ScannedModule[]) =>
  invoke<Project>("add_project", { path, selectedModules });
export const rescanProject = (projectId: string) =>
  invoke<ScannedModule[]>("rescan_project", { projectId });
export const deleteProject = (projectId: string) =>
  invoke<void>("delete_project", { projectId });

// ============================ Service ============================

export const addService = (pomPath: string, name?: string) =>
  invoke<Service>("add_service", { pomPath, name: name ?? null });
export const updateService = (
  id: string,
  name?: string,
  autoRestart?: boolean,
  mavenOpts?: string | null,
  profiles?: string | null,
  devMode?: boolean,
  mainClass?: string | null,
  overrideProperties?: string | null,
) =>
  invoke<void>("update_service", {
    id,
    name: name ?? null,
    autoRestart: autoRestart ?? null,
    mavenOpts: mavenOpts ?? null,
    profiles: profiles ?? null,
    devMode: devMode ?? null,
    mainClass: mainClass ?? null,
    overrideProperties: overrideProperties ?? null,
  });

/// 更新项目级 JDK / Maven 配置
export const updateProjectEnv = (
  projectId: string,
  javaHome?: string | null,
  mavenHome?: string | null
) =>
  invoke<void>("update_project_env", {
    projectId,
    javaHome: javaHome ?? null,
    mavenHome: mavenHome ?? null,
  });
export const deleteService = (id: string) =>
  invoke<void>("delete_service", { id });
export const toggleAutoRestart = (id: string, enabled: boolean) =>
  invoke<void>("toggle_auto_restart", { id, enabled });

// ============================ Process ============================

export const startService = (id: string) => invoke<void>("start_service", { id });
export const stopService = (id: string) => invoke<void>("stop_service", { id });
export const restartService = (id: string) =>
  invoke<void>("restart_service", { id });
export const compileAndStart = (id: string) =>
  invoke<void>("compile_and_start", { id });
export const recompileAndStart = (id: string) =>
  invoke<void>("recompile_and_start", { id });
export const cleanService = (id: string) =>
  invoke<void>("clean_service", { id });
export const stopAll = () => invoke<void>("stop_all");

// 带依赖启动
export const startServiceWithDependencies = (id: string) =>
  invoke<void>("start_service_with_dependencies", { id });

// 批量启动（一键启动项目下所有服务）
export const startServicesBatch = (ids: string[]) =>
  invoke<BatchStartResult>("start_services_batch", { ids });

// 服务依赖编排
export const getServiceDependencies = (id: string) =>
  invoke<string[]>("get_service_dependencies", { id });

export const setServiceDependencies = (id: string, dependsOnIds: string[]) =>
  invoke<void>("set_service_dependencies", { id, dependsOnIds });
export const getRuntime = (id: string) =>
  invoke<ServiceRuntime>("get_runtime", { id });
export const getAllRuntimes = () =>
  invoke<ServiceRuntime[]>("get_all_runtimes");
export const refreshPortConflicts = () => invoke<void>("refresh_port_conflicts");

// ============================ Git ============================

export const gitAvailable = () => invoke<boolean>("git_available");
export const gitPull = (projectId: string) =>
  invoke<PullResult>("git_pull", { projectId });
export const gitPullAndRestart = (projectId: string) =>
  invoke<PullResult>("git_pull_and_restart", { projectId });
export const gitStatus = (projectId: string) =>
  invoke<GitStatus>("git_status", { projectId });
export const gitDiff = (projectId: string, path: string, staged: boolean) =>
  invoke<string>("git_diff", { projectId, path, staged });
export const gitStage = (projectId: string, paths: string[]) =>
  invoke<void>("git_stage", { projectId, paths });
export const gitUnstage = (projectId: string, paths: string[]) =>
  invoke<void>("git_unstage", { projectId, paths });
export const gitCommit = (projectId: string, message: string) =>
  invoke<void>("git_commit", { projectId, message });
export const gitLog = (projectId: string, limit = 50) =>
  invoke<GitCommitInfo[]>("git_log", { projectId, limit });
export const gitShow = (projectId: string, hash: string) =>
  invoke<string>("git_show", { projectId, hash });
export const gitReadFile = (projectId: string, path: string) =>
  invoke<string>("git_read_file", { projectId, path });
export const gitWriteFile = (
  projectId: string,
  path: string,
  content: string,
) =>
  invoke<void>("git_write_file", { projectId, path, content });
/// 读取 HEAD 中某文件内容（未跟踪 / 不存在返回 null），用于行级 diff
export const gitFileHead = (projectId: string, path: string) =>
  invoke<string | null>("git_file_head", { projectId, path });
/// 工作区 vs HEAD 的 diff hunk（unified=0，与 Git 面板同引擎）
export const gitDiffHunks = (projectId: string, path: string) =>
  invoke<{new_start: number; new_lines: number; del_lines: number}[]>(
    "git_diff_hunks",
    { projectId, path }
  );

// ============================ Files（项目文件浏览/编辑） ============================

export const listFiles = (projectId: string, path: string) =>
  invoke<FileEntry[]>("list_files", { projectId, path });
export const readProjectFile = (projectId: string, path: string) =>
  invoke<FileContent>("read_project_file", { projectId, path });
export const writeProjectFile = (
  projectId: string,
  path: string,
  content: string,
) =>
  invoke<void>("write_project_file", { projectId, path, content });

export const getFileAbsPath = (projectId: string, path: string) =>
  invoke<string>("get_file_abs_path", { projectId, path });

/// 重命名文件 / 目录，返回新相对路径
export const fsRename = (projectId: string, path: string, newName: string) =>
  invoke<string>("fs_rename", { projectId, path, newName });

/// 复制文件 / 目录到目标目录，返回新路径
export const fsCopyEntry = (projectId: string, srcPath: string, destDir: string) =>
  invoke<string>("fs_copy_entry", { projectId, srcPath, destDir });

/// 移动文件 / 目录到目标目录，返回新路径
export const fsMoveEntry = (projectId: string, srcPath: string, destDir: string) =>
  invoke<string>("fs_move_entry", { projectId, srcPath, destDir });

/// 在系统文件管理器中定位该条目
export const revealInFileManager = (projectId: string, path: string) =>
  invoke<void>("reveal_in_file_manager", { projectId, path });

// ============================ Terminal（集成终端） ============================

export const terminalCreate = (projectId: string) =>
  invoke<string>("terminal_create", { projectId });
export const terminalWrite = (sessionId: string, data: string) =>
  invoke<void>("terminal_write", { sessionId, data });
export const terminalKill = (sessionId: string) =>
  invoke<void>("terminal_kill", { sessionId });

// ============================ Config ============================

export const getConfig = () => invoke<AppConfig>("get_config");
export const saveConfig = (config: AppConfig) =>
  invoke<void>("save_config", { config });

// ============================ Util ============================

export const openInBrowser = (port: number) =>
  invoke<void>("open_in_browser", { port });

export const detectJdks = () => invoke<JdkInfo[]>("detect_jdks");
export const detectMavens = () => invoke<MavenInfo[]>("detect_mavens");
