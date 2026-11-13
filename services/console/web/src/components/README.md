# Shared console components

Owned by the foundation. Slices use these as-is; ask the coordinator before
changing one. `ui/` holds stock shadcn/ui primitives (new-york style); the
console look comes entirely from the theme variables in `src/index.css`.
Regenerate or add primitives with `pnpm dlx shadcn@latest add <name>` from
`services/console/web`, then fix the `cn` import to `@/lib/utils` if the CLI
rewrites it and drop any `cn`/`next-themes` dependency it adds (the Toaster
reads the console theme from `@/lib/theme`).

Theme and type: dark is the default (`<html class="dark"
data-console-theme="dark">`, set before first paint and by `@/lib/theme`).
Use only the shadcn tokens (`background`, `foreground`, `card`, `popover`,
`primary`, `secondary`, `muted`, `accent`, `destructive`, `border`, `input`,
`ring`, `sidebar-*`, `chart-*`) plus `success`, `warning`, and `info`, with
opacity modifiers for soft tints (`bg-primary/10`). UI text is Geist; use
`font-mono` (Geist Mono) only for IDs, keys, dates, code, and values. Labels,
headings, and table headers are sentence case, never `uppercase`. Interactive
elements are shadcn primitives (Button, Input, Select, Checkbox, RadioGroup,
Switch, Table, Badge, Card, Dialog/AlertDialog, DropdownMenu, Sheet via the
Sidebar, Tooltip, Skeleton, Sonner) and keep the stock focus ring.

| Component | File | Use |
|---|---|---|
| `AppLink` | `app-link.tsx` | Link to any console path. Client-side for SPA routes, full page load for Rails protocol routes (`/oauth/:slug/start`, `/auth/:provider/start`). Use it for cross-slice links. |
| `PageHeader`, `SectionHeading`, `useDocumentTitle` | `page-header.tsx` | Page title/subtitle/actions (legacy `_page_header`); sets `<title>` to "X · Centaur Console". `SectionHeading` is the in-page section title. |
| `BackLink` | `back-link.tsx` | "← back to …" link. |
| `ControlTabs` | `control-tabs.tsx` | The Control tab strip. Render at the top of Control index pages; tabs come from feature `controlTabs`. |
| `RequireAccess` | `require-access.tsx` | Page guard: `allowed={(me) => me.nav.x}`. Silently sends others to their landing page (legacy `require_admin`). The API still enforces access. |
| `DataTable`, `Column` | `data-table.tsx` | Table panel (shadcn `Table`) with header, hover rows, skeleton loading rows, an empty row, and an optional `Pagination` footer from API `meta`. Tables with `fixed` layout or four or more columns keep a minimum width and scroll horizontally on narrow screens; override with `minWidth`. |
| `Pagination`, `usePageParam` | `pagination.tsx` | "N units · page X of Y" with Previous/Next. `usePageParam("roles_page")` keeps several paginators on one page independent in the URL. A response past the last page (`page > total_pages > 0`) replaces the URL's page with the last page, keeping other params; `DataTable` shows loading rows meanwhile. Pass `hideSinglePage` instead of omitting `pagination` so stale pages still clamp. |
| `SearchBox` | `search-box.tsx` | `?q=` search form with Search/Clear; resets `page`. |
| `EmptyState` | `empty-state.tsx` | Dashed empty box. |
| `ErrorAlert`, `WarningAlert` | `alerts.tsx` | Red error box, amber warning box. |
| `ConfirmDialog` | `confirm-dialog.tsx` | Confirmation for destructive actions (replaces `data-turbo-confirm`). `onConfirm` may be async. |
| `CopyButton` | `copy-button.tsx` | Copy a value to the clipboard with feedback. |
| `LocalTime` | `local-time.tsx` | Timestamp in the viewer's zone: `absolute` (MM/DD/YYYY HH:MM:SS), `relative`, or `compact` ("4d"); re-renders every 30s; `—` for null. |
| `ResourceBadge`, `LabelChip(s)`, `RolePill`, `StatusBadge` (+ `credentialStatusTone`, `workflowStatusTone`, `userStatusTone`) | `badges.tsx` | Badges and status pills. |
| `TextField`, `TextareaField`, `SelectField`, `CheckboxField`, `FormSection`, `FieldShell` | `form/fields.tsx` | react-hook-form field patterns (label, hint, inline error). Validate with zod via `zodResolver`. |
| `KeyValueRowsField`, `recordToRows`, `rowsToRecord` | `form/key-value-rows.tsx` | Repeatable key/value rows (labels, headers). Requires `FormProvider`. Blank keys are dropped. |
| `applyApiErrors`, `FormErrorSummary` | `form/form-errors.tsx` | Map a 422 `details` hash onto fields; "X could not be saved." summary. |
| `SecretField`, `omitBlankSecrets` | `form/secret-field.tsx` | Write-only secret input: never prefilled; with `hasStoredValue` it reads "•••••••• (unchanged)" and a blank value is omitted from the request so the stored value is kept. |
| `UserTypeahead` | `user-typeahead.tsx` | Multi-user picker with a filterable listbox; values are user ids. |
| `SlackChannelPicker`, `adminSlackChannelLoader`, `useSlackChannelNames` | `slack-channel-picker.tsx` | Debounced Slack channel combobox with manual ID entry. `adminSlackChannelLoader("principal:<oid>")` searches `GET /api/v1/console/slack_channels`; pass your own loader for other policies (scheduled tasks). `useSlackChannelNames(ids)` resolves catalog names and the catalog warning. Remount with a new `key` to clear. |
| `SlackChannelPermissionsEditor` | `slack-channel-permissions-editor.tsx` | The Slack channel permissions section (rows with Upload/Download/History, delete, add row, inherited table). Props: `permissions`, `inherited`, `channelNames`, `catalogError`, `directMessageOwnerName` (principals: DM rows read-only), `loadOptions`, `onSave(rows)` (full replacement), `onDelete(channelId)`. |
| `SecretGrantPicker` | `secret-grant-picker.tsx` | Searchable, kind-grouped secret picker over `GET /api/v1/console/secrets`, with `excludeIds`. |

Related modules outside this folder:

- `@/lib/api` — `api.get/post/put/patch/delete`, `ApiError` (`status`,
  `reason`, `fieldErrors`), `errorMessage`. Only accepts paths under
  `/api/v1/console/` (`CONSOLE_API_PREFIX`, the session-only console API;
  the rest of `/api/v1` is the bearer API). Sends `X-CSRF-Token` on writes,
  redirects to `/login` on 401 and `/pending` for pending accounts, retries
  once on a stale CSRF token.
- `@/lib/query-keys` — `createQueryKeys("<slice>")` and `sharedKeys`.
- `@/lib/format` — `truncateMiddle`, `pluralize`, `displayLabel`, time formats.
- `@/lib/secret-kinds` — secret kind registry, `grantKey`, `secretOptionLabel`,
  `SOURCE_TYPE_LABELS`.
- `@/app/session` — `useSession()` (`Me` from `GET /api/v1/console/me`),
  `defaultLandingPath`.
- `@/lib/use-submit` — `useSubmit(fn)`: `run`, `pending`, `error` for one
  write, held in component state.
- `@/test/utils` — `mockApi`, `apiResponse`, `renderRoute`, `adminMe`,
  `operatorMe` for the SPA tests.
- Toasts: `import { toast } from "sonner"` (`toast.success` for the legacy
  flash notice, `toast.error` for alerts).

## Reads and writes

- Reads use TanStack Query (`useQuery`, `useSuspenseQuery`) with keys from
  `createQueryKeys`. A failed query renders an error alert, never an empty
  state.
- Writes are plain exported async functions in the feature's `api.ts`, never
  hooks: `export async function updateRole(queryClient, id, data)` calls `api`,
  then `queryClient.invalidateQueries` on the narrowest affected keys, and
  returns the response. The component awaits it, shows the toast, and keeps
  pending/error state in `useState`, react-hook-form's `isSubmitting`, or
  `useSubmit`. 422 field errors go through `applyApiErrors` and
  `FormErrorSummary`.
- `useMutation` is banned by ESLint (`no-restricted-imports`): request bodies
  can carry secrets and must never be stored in a TanStack cache. Never call
  `setQueryData` with a value that holds a secret.
- Secret-bearing forms (secret inline values, credential secrets and header
  values, the OAuth app client secret, the SSH private key) clear those fields
  in a `finally` after every submit. An issued API client secret lives only in
  route-scoped component state (`features/admin/secret-handoff.tsx`).

## Tests

The SPA keeps tests for logic and for secret handling only: `lib/api`,
`lib/format`, `LocalTime`, stale-page clamping in `Pagination`, the Slack channel picker's saved label, the secret form model, `SecretField`, the shell,
navigation, and `features/secret-handling.test.tsx` (plaintext gone from the
inputs, the page, and both TanStack caches after each secret-bearing submit,
and the show-once API client secret). Page behaviour is covered by the Rails
request tests and a browser pass.
