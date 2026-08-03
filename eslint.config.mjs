import eslint from "@eslint/js";

export default [
  {
    ignores: ["**/node_modules/**", "**/dist/**"],
  },
  {
    files: ["crates/harn-cli/src/commands/app_host/*.js"],
    ...eslint.configs.recommended,
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "script",
      globals: {
        Array: "readonly",
        URLSearchParams: "readonly",
        console: "readonly",
        document: "readonly",
        encodeURIComponent: "readonly",
        fetch: "readonly",
        innerHeight: "readonly",
        innerWidth: "readonly",
        Intl: "readonly",
        location: "readonly",
        matchMedia: "readonly",
        navigator: "readonly",
        parent: "readonly",
        structuredClone: "readonly",
        window: "readonly",
        Worker: "readonly",
        __HARN_SANDBOX_ORIGIN__: "readonly",
        __HARN_TITLE__: "readonly",
        __HARN_VERSION__: "readonly",
      },
    },
    rules: {
      ...eslint.configs.recommended.rules,
      curly: ["error", "all"],
    },
  },
  {
    files: ["crates/harn-cli/src/commands/app_host/portable_worker.js"],
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      globals: {
        crypto: "readonly",
        self: "readonly",
        Uint8Array: "readonly",
      },
    },
  },
  {
    files: ["scripts/*app_host*.mjs"],
    ...eslint.configs.recommended,
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      globals: {
        structuredClone: "readonly",
        URL: "readonly",
        URLSearchParams: "readonly",
      },
    },
    rules: {
      ...eslint.configs.recommended.rules,
      curly: ["error", "all"],
    },
  },
];
