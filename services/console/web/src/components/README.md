# Shared console components

Owned by the foundation. Slices use these as-is; ask the coordinator before
changing one. `ui/` holds shadcn/ui primitives (restyled for the console
palette); regenerate or add primitives with
`pnpm dlx shadcn@latest add <name>` from `services/console/web`, then fix the
`cn` import to `@/lib/utils` if the CLI rewrites it.

| Component | File | Use |
|---|---|---|
| `AppLink` | `app-link.tsx` | Link to any console path. Client-side for SPA routes, full page load for Rails protocol routes (`/oauth/:slug/start`, `/auth/:provider/start`). Use it for cross-slice links. |
| `PageHeader`, `SectionHeading`, `useDocumentTitle` | `page-header.tsx` | Page title/subtitle/actions (legacy `_page_header`); sets `<title>` to "X · Centaur Console". Section headings are the small uppercase labels. |
| `BackLink` | `back-link.tsx` | "← back to …" link. |
| `ControlTabs` | `control-tabs.tsx` | The Control tab strip. Render at the top of Control index pages; tabs come from feature `controlTabs`. |
| `RequireAccess` | `require-access.tsx` | Page guard: `allowed={(me) => me.nav.x}`. Silently sends others to their landing page (legacy `require_admin`). The API still enforces access. |
| `DataTable`, `Column` | `data-table.tsx` | Table panel with header, hover rows, loading and empty rows, and an optional `Pagination` footer from API `meta`. |
| `Pagination`, `usePageParam` | `pagination.tsx` | "N units · page X of Y" with Previous/Next. `usePageParam("roles_page")` keeps several paginators on one page independent in the URL. |
| `SearchBox` | `search-box.tsx` | `?q=` search form with Search/Clear; resets `page`. |
| `EmptyState` | `empty-state.tsx` | Dashed empty box. |
| `ErrorAlert`, `WarningAlert` | `alerts.tsx` | Red error box, amber warning box. |
| `ConfirmDialog` | `confirm-dialog.tsx` | Confirmation for destructive actions (replaces `data-turbo-confirm`). `onConfirm` may be async. |
| `CopyButton` | `copy-button.tsx` | Copy a value to the clipboard with feedback. |
| `LocalTime` | `local-time.tsx` | Timestamp in the viewer's zone: `absolute` (MM/DD/YYYY HH:MM:SS), `relative`, or `compact` ("4d"); re-renders every 30s; `—` for null. |
| `ResourceBadge`, `LabelChip(s)`, `RolePill`, `StatusBadge` (+ `credentialStatusTone`, `workflowStatusTone`, `userStatusTone`) | `badges.tsx` | Badges and status pills. |
| `TextField`, `TextareaField`, `SelectField`, `CheckboxField`, `FormSection`, `FieldShell` | `form/fields.tsx` | react-hook-form field patterns (uppercase label, hint, inline error). Validate with zod via `zodResolver`. |
| `KeyValueRowsField`, `recordToRows`, `rowsToRecord` | `form/key-value-rows.tsx` | Repeatable key/value rows (labels, headers). Requires `FormProvider`. Blank keys are dropped. |
| `applyApiErrors`, `FormErrorSummary` | `form/form-errors.tsx` | Map a 422 `details` hash onto fields; "X could not be saved." summary. |
| `SecretField`, `omitBlankSecrets` | `form/secret-field.tsx` | Write-only secret input: never prefilled; with `hasStoredValue` it reads "•••••••• (unchanged)" and a blank value is omitted from the request so the stored value is kept. |
| `UserTypeahead` | `user-typeahead.tsx` | Multi-user picker with a filterable listbox; values are user ids. |
| `SlackChannelPicker`, `adminSlackChannelLoader`, `useSlackChannelNames` | `slack-channel-picker.tsx` | Debounced Slack channel combobox with manual ID entry. `adminSlackChannelLoader("principal:<oid>")` searches `GET /api/v1/slack_channels`; pass your own loader for other policies (scheduled tasks). `useSlackChannelNames(ids)` resolves catalog names and the catalog warning. Remount with a new `key` to clear. |
| `SlackChannelPermissionsEditor` | `slack-channel-permissions-editor.tsx` | The Slack channel permissions section (rows with Upload/Download/History, delete, add row, inherited table). Props: `permissions`, `inherited`, `channelNames`, `catalogError`, `directMessageOwnerName` (principals: DM rows read-only), `loadOptions`, `onSave(rows)` (full replacement), `onDelete(channelId)`. |
| `SecretGrantPicker` | `secret-grant-picker.tsx` | Searchable, kind-grouped secret picker over `GET /api/v1/secrets`, with `excludeIds`. |

Related modules outside this folder:

- `@/lib/api` — `api.get/post/put/patch/delete`, `ApiError` (`status`,
  `reason`, `fieldErrors`), `errorMessage`. Sends `X-CSRF-Token` on writes,
  redirects to `/login` on 401 and `/pending` for pending accounts, retries
  once on a stale CSRF token.
- `@/lib/query-keys` — `createQueryKeys("<slice>")` and `sharedKeys`.
- `@/lib/format` — `truncateMiddle`, `pluralize`, `displayLabel`, time formats.
- `@/lib/secret-kinds` — secret kind registry, `grantKey`, `secretOptionLabel`,
  `SOURCE_TYPE_LABELS`.
- `@/app/session` — `useSession()` (`Me` from `GET /api/v1/me`),
  `defaultLandingPath`.
- `@/test/utils` — `mockApi`, `apiResponse`, `renderRoute`, `adminMe`,
  `operatorMe` for page tests.
- Toasts: `import { toast } from "sonner"` (`toast.success` for the legacy
  flash notice, `toast.error` for alerts).
