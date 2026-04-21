import js from "@eslint/js"
import reactHooks from "eslint-plugin-react-hooks"
import reactRefresh from "eslint-plugin-react-refresh"
import tseslint from "typescript-eslint"

export default tseslint.config(
  {
    ignores: ["dist", "../portal-dist"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      parserOptions: {
        ecmaFeatures: {
          jsx: true,
        },
      },
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // eslint-plugin-react-hooks v7.1 added `set-state-in-effect` as a
      // recommended error. The rule flags patterns like `void loadRuns()`
      // inside an effect whose callback eventually calls setState — a
      // legitimate "load-on-mount / load-on-deps-change" idiom in this
      // codebase. Downgrade until we adopt the React 19+ Effect Event API
      // or actions-based refactor for each call site (tracked separately).
      "react-hooks/set-state-in-effect": "off",
      curly: ["error", "all"],
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
    },
  },
)
