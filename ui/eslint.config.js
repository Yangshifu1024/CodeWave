// CodeWave 前端 lint 配置（2026-09-19 引入：此前仓库没有任何前端 lint 工具）
//
// 规则集取「最小够用」：eslint 推荐 + typescript-eslint 推荐 + react-hooks 的两条经典正确性规则。
// 有意没启用 react-hooks v7「recommended-latest」里的 React Compiler 系规则
// （static-components / use-memo / immutability / preserve-manual-memoization 等）：
// 那套规则要求按编译器语义重构组件，与现有写法冲突面大，留作后续独立批次评估。
import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: ["dist/**", "node_modules/**", "src/**/*.d.ts"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2024,
      sourceType: "module",
      globals: { ...globals.browser },
    },
    plugins: { "react-hooks": reactHooks },
    // 没起作用的 eslint-disable 注释也算问题，避免留下失效豁免
    linterOptions: { reportUnusedDisableDirectives: "error" },
    rules: {
      // 有意关闭（2026-09-19 引入 eslint 时的现状记录）：本仓 IPC 边界
      // （ipc/types.ts、ipc/client.ts、ipc/events.ts、stores/run*.ts 等）与测试替身
      // 大量用 any 表达「后端 JSON 载荷尚未建模」，共 307 处（测试文件 225 / 业务代码 82）。
      // 逐处换成真实类型等于给 IPC 事件载荷做一次完整建模，属独立专项批次；
      // 在那之前挂着这条检查只会常年飘红、失去信号价值。
      "@typescript-eslint/no-explicit-any": "off",
      // 下划线前缀 = 有意保留的占位形参（测试替身按签名对齐用），按惯例不报
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
    },
  },
);
