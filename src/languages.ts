// 文件扩展名 → Prism 语言映射 + 语言组件加载
// 覆盖 Java/Spring Boot 项目中常见的文件类型

const EXT_LANG_MAP: Record<string, string> = {
  // Java
  java: "java",
  // JVM
  kt: "kotlin",
  kts: "kotlin",
  groovy: "groovy",
  gradle: "groovy",
  // Web / 前端
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "jsx",
  ts: "typescript",
  tsx: "tsx",
  vue: "markup",
  html: "markup",
  htm: "markup",
  xml: "markup",
  svg: "markup",
  css: "css",
  scss: "scss",
  sass: "sass",
  less: "less",
  // 配置 / 数据
  json: "json",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  ini: "ini",
  cfg: "ini",
  conf: "ini",
  properties: "properties",
  env: "properties",
  // 脚本 / 构建
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  bat: "bash",
  cmd: "bash",
  ps1: "powershell",
  // 数据库
  sql: "sql",
  // 文档
  md: "markdown",
  markdown: "markdown",
  // 其他
  dockerfile: "docker",
  gitignore: "ignore",
  python: "python",
  py: "python",
  go: "go",
  rs: "rust",
  c: "c",
  cpp: "cpp",
  h: "c",
  hpp: "cpp",
};

const MD_EXTS = new Set(["md", "markdown"]);

/** 获取文件扩展名（小写，不含点） */
export function getExt(filename: string): string {
  const dot = filename.lastIndexOf(".");
  if (dot < 0) return "";
  // 处理 dotfile 如 .gitignore
  if (dot === 0) return filename.slice(1).toLowerCase();
  return filename.slice(dot + 1).toLowerCase();
}

/** 是否为 Markdown 文件 */
export function isMarkdown(filename: string): boolean {
  return MD_EXTS.has(getExt(filename));
}

/** 获取 Prism 语言名，未识别返回 null */
export function getPrismLang(filename: string): string | null {
  const ext = getExt(filename);
  // 特殊文件名处理
  const base = filename.split("/").pop() ?? filename;
  const lowerBase = base.toLowerCase();
  if (lowerBase === "dockerfile") return "docker";
  if (lowerBase === ".gitignore" || lowerBase === ".gitattributes") return "ignore";
  if (lowerBase === "makefile") return "makefile";
  return EXT_LANG_MAP[ext] ?? null;
}