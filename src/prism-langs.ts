// Prism 语言组件按需注册
// 注意：Prism 组件之间存在依赖关系，必须按依赖顺序 import（父组件在前）
import Prism from "prismjs";

// clike 是 C 系语言基础
import "prismjs/components/prism-clike";
// markup（HTML/XML）与其模板渲染器
import "prismjs/components/prism-markup";
import "prismjs/components/prism-markup-templating";
// Web 前端
import "prismjs/components/prism-javascript";
import "prismjs/components/prism-css";
import "prismjs/components/prism-scss";
import "prismjs/components/prism-sass";
import "prismjs/components/prism-less";
import "prismjs/components/prism-jsx";
import "prismjs/components/prism-typescript";
import "prismjs/components/prism-tsx";
// JVM / Java 生态
import "prismjs/components/prism-java";
import "prismjs/components/prism-kotlin";
import "prismjs/components/prism-groovy";
// 配置 / 数据 / 脚本
import "prismjs/components/prism-json";
import "prismjs/components/prism-yaml";
import "prismjs/components/prism-toml";
import "prismjs/components/prism-ini";
import "prismjs/components/prism-properties";
import "prismjs/components/prism-bash";
import "prismjs/components/prism-powershell";
import "prismjs/components/prism-sql";
// 文档
import "prismjs/components/prism-markdown";
// 工程 / 系统
import "prismjs/components/prism-docker";
import "prismjs/components/prism-ignore";
import "prismjs/components/prism-makefile";
import "prismjs/components/prism-python";
import "prismjs/components/prism-go";
import "prismjs/components/prism-rust";
import "prismjs/components/prism-c";
import "prismjs/components/prism-cpp";

export { Prism };