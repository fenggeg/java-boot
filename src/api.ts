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
export const gitPush = (projectId: string) =>
  invoke<PullResult>("git_push", { projectId });
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
// 单文件提交历史（--follow 跟随重命名），编辑器「文件历史 / 回滚」浮层用
export const gitFileLog = (projectId: string, path: string, limit = 100) =>
  invoke<GitCommitInfo[]>("git_file_log", { projectId, path, limit });
// 读取指定提交中某文件的内容（历史预览 / 整文件回滚）
export const gitShowFile = (projectId: string, hash: string, path: string) =>
  invoke<string>("git_show_file", { projectId, hash, path });
export const gitReadFile = (projectId: string, path: string) =>
  invoke<string>("git_read_file", { projectId, path });
export const gitWriteFile = (
  projectId: string,
  path: string,
  content: string,
) =>
  invoke<void>("git_write_file", { projectId, path, content });
/** HEAD 文件信息 + 行级标记抑制标志（ignored / skip-worktree 时 suppress=true） */
export interface FileHeadInfo {
  /** HEAD 中内容；未跟踪 / 不在 HEAD 为 null */
  head: string | null;
  /** true = 不显示行级 diff 标记（与 Git 面板口径一致） */
  suppress: boolean;
}

/// 读取 HEAD 中某文件内容（含 ignored / skip-worktree 抑制判定），用于行级 diff
export const gitFileHead = (projectId: string, path: string) =>
  invoke<FileHeadInfo>("git_file_head", { projectId, path });
/// 工作区 vs HEAD 的 diff hunk（unified=0，与 Git 面板同引擎）
export const gitDiffHunks = (projectId: string, path: string) =>
  invoke<{new_start: number; new_lines: number; del_lines: number}[]>(
    "git_diff_hunks",
    { projectId, path }
  );

// ---- 冲突合并 ----

/** 冲突文件三方版本 */
export interface ConflictVersions {
  /** 共同祖先（双方新增时为 null） */
  base: string | null;
  /** 本地版本 */
  ours: string;
  /** 远程版本 */
  theirs: string;
}

/** 冲突文件三方内容（base / ours / theirs） */
export const gitConflictVersions = (projectId: string, path: string) =>
  invoke<ConflictVersions>("git_conflict_versions", { projectId, path });

/** 快捷采用某侧解决冲突：ours / theirs / both */
export const gitResolveSide = (
  projectId: string,
  path: string,
  side: "ours" | "theirs" | "both"
) => invoke<void>("git_resolve_side", { projectId, path, side });

/** 标记冲突已解决：写回编辑后的内容并暂存 */
export const gitMarkResolved = (
  projectId: string,
  path: string,
  content: string
) => invoke<void>("git_mark_resolved", { projectId, path, content });

/** 全部冲突解决后完成合并提交（message 为空用默认合并信息） */
export const gitCompleteMerge = (projectId: string, message?: string | null) =>
  invoke<void>("git_complete_merge", { projectId, message: message ?? null });

/** 中止本次合并，恢复合并前状态 */
export const gitAbortMerge = (projectId: string) =>
  invoke<void>("git_abort_merge", { projectId });

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

/** 项目内全量文件扁平条目（快速打开用，已排除依赖/构建目录与符号链接） */
export interface FlatFile {
  path: string;
  name: string;
}

/// 扁平遍历项目内全部文件（快速打开数据源，上限 5 万条）
export const walkFiles = (projectId: string) =>
  invoke<FlatFile[]>("walk_files", { projectId });

// ============================ Terminal（集成终端） ============================

export const terminalCreate = (projectId: string) =>
  invoke<string>("terminal_create", { projectId });
export const terminalWrite = (sessionId: string, data: string) =>
  invoke<void>("terminal_write", { sessionId, data });
export const terminalResize = (
  sessionId: string,
  cols: number,
  rows: number
) => invoke<void>("terminal_resize", { sessionId, cols, rows });
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
