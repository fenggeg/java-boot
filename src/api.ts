import {invoke} from "@tauri-apps/api/core";
import type {
    AppConfig,
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
export const stopAll = () => invoke<void>("stop_all");
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

// ============================ Config ============================

export const getConfig = () => invoke<AppConfig>("get_config");
export const saveConfig = (config: AppConfig) =>
  invoke<void>("save_config", { config });

// ============================ Util ============================

export const openInBrowser = (port: number) =>
  invoke<void>("open_in_browser", { port });

export const detectJdks = () => invoke<JdkInfo[]>("detect_jdks");
export const detectMavens = () => invoke<MavenInfo[]>("detect_mavens");
