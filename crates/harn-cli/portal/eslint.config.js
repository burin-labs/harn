import js from "@eslint/js"
import reactHooks from "eslint-plugin-react-hooks"
import reactRefresh from "eslint-plugin-react-refresh"
import tseslint from "typescript-eslint"

export default tseslint.config(
  {
    ignores: ["dist", "../portal-dist"],
  },
  js.configs.recommended,
  // strict + stylistic raise the floor above plain `recommended` without
  // requiring type-aware linting (no parserOptions.project plumbing). Type-
  // checked rules can be layered on later by switching to the *TypeChecked
  // variants and wiring tsconfig.json under parserOptions.
  ...tseslint.configs.strict,
  ...tseslint.configs.stylistic,
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
      // Stylistic-only rules whose enforcement would churn the existing
      // `type X = ...` and `Array<T>` usage without any correctness gain.
      "@typescript-eslint/consistent-type-definitions": "off",
      "@typescript-eslint/array-type": "off",
    },
  },
  {
    // Tests need `() => {}` stub callbacks and `getByText(...)!` assertions
    // routinely; the lints don't add safety value in jsdom test setups.
    files: ["src/**/*.test.{ts,tsx}", "src/**/test/**"],
    rules: {
      "@typescript-eslint/no-empty-function": "off",
      "@typescript-eslint/no-non-null-assertion": "off",
    },
  },
)
