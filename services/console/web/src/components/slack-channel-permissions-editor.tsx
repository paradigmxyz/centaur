import { Trash2 } from "lucide-react";
import { useMemo, useState } from "react";

import { WarningAlert } from "@/components/alerts";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { SlackChannelPicker, type SlackChannelLoader } from "@/components/slack-channel-picker";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";

/** One permission row as the API serializes it (slack_channel_permissions_payload). */
export interface SlackChannelPermissionRow {
  channel_id: string;
  upload_enabled: boolean;
  download_enabled: boolean;
  history_enabled: boolean;
}

type Flag = "upload_enabled" | "download_enabled" | "history_enabled";

/** SlackChannelPermission::PERMISSION_FLAGS */
export const SLACK_PERMISSION_FLAGS: { attribute: Flag; label: string }[] = [
  { attribute: "upload_enabled", label: "Upload" },
  { attribute: "download_enabled", label: "Download" },
  { attribute: "history_enabled", label: "History" },
];

const DEFAULT_FLAGS: Record<Flag, boolean> = { upload_enabled: true, download_enabled: true, history_enabled: true };

export interface SlackChannelPermissionsEditorProps {
  /** The owner's direct permissions. */
  permissions: SlackChannelPermissionRow[];
  /** Read-only permissions inherited from roles (principals only). */
  inherited?: SlackChannelPermissionRow[];
  /** channel_id -> catalog name, for display. */
  channelNames?: Record<string, string>;
  /** Catalog warning (missing token, catalog loading). */
  catalogError?: string | null;
  /**
   * Principals: DM rows (channel ids starting with "D") are API-managed and
   * shown read-only with this display name ("DM <name>").
   */
  directMessageOwnerName?: string;
  loadOptions: SlackChannelLoader;
  /**
   * Persist the full replacement set (excluding API-managed DM rows, which the
   * server preserves). Resolve on success; throw to keep the draft.
   */
  onSave: (rows: SlackChannelPermissionRow[]) => Promise<unknown>;
  /** Delete one direct permission. */
  onDelete: (channelId: string) => Promise<unknown>;
}

const isDirectMessage = (channelId: string) => channelId.toUpperCase().startsWith("D");

/**
 * The Slack channel permissions section shared by the principal and role
 * pages: editable flags per channel, delete per row, an add row with the
 * channel picker, and the inherited-from-roles table.
 */
export function SlackChannelPermissionsEditor({
  permissions,
  inherited = [],
  channelNames = {},
  catalogError,
  directMessageOwnerName,
  loadOptions,
  onSave,
  onDelete,
}: SlackChannelPermissionsEditorProps) {
  const managesDms = directMessageOwnerName !== undefined;
  const [draft, setDraft] = useState<SlackChannelPermissionRow[]>(permissions);
  const [newChannel, setNewChannel] = useState("");
  const [newFlags, setNewFlags] = useState(DEFAULT_FLAGS);
  const [pickerValid, setPickerValid] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [pickerKey, setPickerKey] = useState(0);

  // Reset the draft whenever fresh permissions arrive from the server.
  const [source, setSource] = useState(permissions);
  if (source !== permissions) {
    setSource(permissions);
    setDraft(permissions);
  }

  const existingIds = useMemo(() => new Set(permissions.map((row) => row.channel_id)), [permissions]);

  const setFlag = (channelId: string, flag: Flag, value: boolean) =>
    setDraft((rows) => rows.map((row) => (row.channel_id === channelId ? { ...row, [flag]: value } : row)));

  const save = async () => {
    setError(null);
    const rows = draft.filter((row) => !(managesDms && isDirectMessage(row.channel_id)));
    if (newChannel) {
      if (existingIds.has(newChannel)) {
        setError("Each Slack channel can only be selected once.");
        return;
      }
      rows.push({ channel_id: newChannel, ...newFlags });
    }
    setSaving(true);
    try {
      await onSave(rows);
      setNewChannel("");
      setNewFlags(DEFAULT_FLAGS);
      setPickerKey((key) => key + 1);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not save Slack channel permissions.");
    } finally {
      setSaving(false);
    }
  };

  const channelLabel = (channelId: string) => {
    const name = channelNames[channelId];
    return name ? `#${name}` : channelId;
  };

  return (
    <section className="mb-8">
      <div className="mb-4">
        <SectionHeading>Slack Channel Permissions</SectionHeading>
      </div>
      <div className="rounded-sm border border-border bg-card">
        {catalogError && <WarningAlert className="m-4">{catalogError}</WarningAlert>}
        {draft.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="min-w-full text-sm">
              <thead className="border-b border-border bg-table-head text-left text-xs tracking-wider text-muted-foreground uppercase">
                <tr>
                  <th scope="col" className="px-5 py-3 font-medium">
                    Channel
                  </th>
                  {SLACK_PERMISSION_FLAGS.map((flag) => (
                    <th key={flag.attribute} scope="col" className="px-5 py-3 font-medium">
                      {flag.label}
                    </th>
                  ))}
                  <th scope="col" className="px-5 py-3 font-medium">
                    Delete
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border/70">
                {draft.map((row) => {
                  const apiManaged = managesDms && isDirectMessage(row.channel_id);
                  const name = channelNames[row.channel_id];
                  return (
                    <tr key={row.channel_id}>
                      <td className="min-w-80 px-5 py-3">
                        <div className="text-sm text-heading">
                          {apiManaged ? `DM ${directMessageOwnerName || row.channel_id}` : channelLabel(row.channel_id)}
                        </div>
                        {(apiManaged || name) && <div className="mt-1 text-xs text-muted-foreground">{row.channel_id}</div>}
                      </td>
                      {SLACK_PERMISSION_FLAGS.map((flag) => (
                        <td key={flag.attribute} className="px-5 py-3">
                          <Checkbox
                            aria-label={`${flag.label} for ${row.channel_id}`}
                            checked={row[flag.attribute]}
                            disabled={apiManaged}
                            onCheckedChange={(checked) => setFlag(row.channel_id, flag.attribute, checked === true)}
                          />
                        </td>
                      ))}
                      <td className="px-5 py-3">
                        {apiManaged ? (
                          <span className="text-xs text-muted-foreground">API-managed</span>
                        ) : (
                          <ConfirmDialog
                            title={`Delete ${channelLabel(row.channel_id)}?`}
                            description="The owner loses these Slack permissions for this channel."
                            confirmLabel="Delete"
                            onConfirm={() => onDelete(row.channel_id)}
                            trigger={
                              <Button
                                type="button"
                                variant="ghost"
                                size="icon-sm"
                                title={`Delete ${row.channel_id}`}
                                aria-label={`Delete ${row.channel_id}`}
                                className="text-muted-foreground hover:bg-red-500/10 hover:text-red-300"
                              >
                                <Trash2 />
                              </Button>
                            }
                          />
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="p-4">
            <EmptyState>No Slack channels selected.</EmptyState>
          </div>
        )}

        <div className="grid gap-4 border-t border-border p-5 lg:grid-cols-[minmax(0,1fr)_auto_auto_auto]">
          <div>
            <label className="mb-1 block text-xs font-semibold tracking-wider text-foreground uppercase" htmlFor="slack-channel-add">
              Add Channel
            </label>
            <SlackChannelPicker
              key={pickerKey}
              id="slack-channel-add"
              loadOptions={loadOptions}
              value={newChannel}
              onChange={setNewChannel}
              onValidityChange={setPickerValid}
            />
          </div>
          {SLACK_PERMISSION_FLAGS.map((flag) => (
            <label key={flag.attribute} className="flex items-start gap-3 pt-6">
              <Checkbox
                checked={newFlags[flag.attribute]}
                aria-label={`${flag.label} for new channel`}
                onCheckedChange={(checked) => setNewFlags((flags) => ({ ...flags, [flag.attribute]: checked === true }))}
              />
              <span className="text-sm font-medium text-heading">{flag.label}</span>
            </label>
          ))}
        </div>
        {error && (
          <p role="alert" className="px-5 pb-3 text-sm text-red-300 light:text-red-700">
            {error}
          </p>
        )}
        <div className="px-5 pb-5">
          <Button type="button" disabled={!pickerValid || saving} onClick={() => void save()}>
            Save Slack channel permissions
          </Button>
        </div>
      </div>

      {inherited.length > 0 && (
        <>
          <div className="mt-6 mb-3">
            <h3 className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">Inherited From Roles</h3>
          </div>
          <div className="overflow-x-auto rounded-sm border border-border bg-card">
            <table className="min-w-full text-sm">
              <thead className="border-b border-border bg-table-head text-left text-xs tracking-wider text-muted-foreground uppercase">
                <tr>
                  <th scope="col" className="px-5 py-3 font-medium">
                    Channel
                  </th>
                  {SLACK_PERMISSION_FLAGS.map((flag) => (
                    <th key={flag.attribute} scope="col" className="px-5 py-3 font-medium">
                      {flag.label}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody className="divide-y divide-border/70">
                {inherited.map((row) => (
                  <tr key={row.channel_id}>
                    <td className="px-5 py-3">
                      <div className="text-sm text-heading">{channelLabel(row.channel_id)}</div>
                      <div className="mt-1 text-xs text-muted-foreground">{row.channel_id}</div>
                    </td>
                    {SLACK_PERMISSION_FLAGS.map((flag) => (
                      <td key={flag.attribute} className="px-5 py-3">
                        <Checkbox
                          checked={row[flag.attribute]}
                          disabled
                          aria-label={`${flag.label} for ${row.channel_id} (inherited)`}
                        />
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </section>
  );
}
