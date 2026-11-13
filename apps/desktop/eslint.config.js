import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";

/**
 * 最小 lint 集：只开 react-hooks 的调用规则。
 * 0.4.0 曾把 useEffect 误放进 useMemo 回调导致启动白屏，
 * 这条规则能在 CI 阶段直接拦住同类错位。
 */
export default [
  {
    files: ["src/**/*.{ts,tsx}"],
    ignores: ["dist/**", "node_modules/**", "src-tauri/**"],
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: {
        ecmaVersion: "latest",
        sourceType: "module",
        ecmaFeatures: { jsx: true },
      },
    },
    plugins: { "react-hooks": reactHooks },
    rules: {
      "react-hooks/rules-of-hooks": "error",
    },
  },
];
