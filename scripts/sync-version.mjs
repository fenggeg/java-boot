// 同步版本号到所有发布相关文件：
//   node scripts/sync-version.mjs 0.23.0
// 会更新 package.json / package-lock.json / tauri.conf.json /
// src-tauri/Cargo.toml / src-tauri/Cargo.lock 中本项目的版本。
import {readFileSync, writeFileSync} from "node:fs";
import {resolve} from "node:path";
import {fileURLToPath} from "node:url";

const version = process.argv[2];
if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version ?? "")) {
  console.error("用法: node scripts/sync-version.mjs <x.y.z>");
  process.exit(1);
}

// Node 18 兼容（import.meta.dirname 自 20.11 起才有）
const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const verRe = /("version":\s*")[^"]+(")/g;

// 每项的 replace 返回 [新内容, 替换次数]
const edits = [
  ["package.json", (s) => {
    let n = 0;
    return [s.replace(verRe, (m, a, b) => (++n === 1 ? a + version + b : m)), n];
  }, 1],
  // package-lock 顶层 + packages[""] 两处；依赖自身的 version 不能动
  ["package-lock.json", (s) => {
    let n = 0;
    return [s.replace(verRe, (m, a, b) => (++n <= 2 ? a + version + b : m)), Math.min(n, 2)];
  }, 2],
  ["src-tauri/tauri.conf.json", (s) => {
    let n = 0;
    return [s.replace(verRe, (m, a, b) => (++n === 1 ? a + version + b : m)), n];
  }, 1],
  // Cargo.toml [package] 下的 version
  ["src-tauri/Cargo.toml", (s) => {
    let n = 0;
    return [s.replace(/(^version\s*=\s*")[^"]+(")/m, (m, a, b) => { n += 1; return a + version + b; }), n];
  }, 1],
  // Cargo.lock 中仅本 crate（避免误改 base64 等同版本依赖）
  ["src-tauri/Cargo.lock", (s) => {
    let n = 0;
    return [s.replace(/(name = "javaboot-launcher"\nversion = ")[^"]+(")/, (m, a, b) => { n += 1; return a + version + b; }), n];
  }, 1],
];

for (const [file, transform, expected] of edits) {
  const path = resolve(root, file);
  const [after, count] = transform(readFileSync(path, "utf8"));
  if (count !== expected) {
    console.error(`${file}: 期望替换 ${expected} 处，实际 ${count} 处，已跳过`);
    process.exitCode = 1;
    continue;
  }
  writeFileSync(path, after, "utf8");
  console.log(`${file}: ${count} 处 → ${version}`);
}
