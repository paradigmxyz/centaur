import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "node_modules", "coverage"] },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.strict],
    languageOptions: {
      ecmaVersion: 2023,
      globals: globals.browser,
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
      // Reads use TanStack Query; writes are plain async functions in each
      // feature's api.ts, so request bodies (which may carry secrets) never
      // enter a TanStack cache.
      "no-restricted-imports": [
        "error",
        {
          paths: [
            {
              name: "@tanstack/react-query",
              importNames: ["useMutation", "useMutationState", "useIsMutating", "MutationCache"],
              message:
                "Writes are plain async functions in the feature's api.ts (apiRequest, then queryClient.invalidateQueries); track pending/error with useState or useSubmit from @/lib/use-submit. See src/components/README.md.",
            },
          ],
        },
      ],
    },
  },
  {
    // Route pages are the hot-reload boundary; shared modules may export
    // helpers next to components.
    files: ["src/features/*/pages/**/*.tsx"],
    rules: { "react-refresh/only-export-components": ["warn", { allowConstantExport: true }] },
  },
);
