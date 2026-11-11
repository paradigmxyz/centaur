import { Trash2 } from "lucide-react";
import { useMemo, useState } from "react";

import { WarningAlert } from "@/components/alerts";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { SlackChannelPicker, type SlackChannelLoader } from "@/components/slack-channel-picker";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

const headClass = "h-10 px-5 text-xs font-medium text-muted-foreground";

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
      <div className="mb-3">
        <SectionHeading>Slack Channel Permissions</SectionHeading>
      </div>
      <Card className="gap-0 overflow-hidden py-0">
        {catalogError && (
          <div className="p-4 pb-0">
            <WarningAlert>{catalogError}</WarningAlert>
          </div>
        )}
        {draft.length > 0 ? (
          <Table>
            <TableHeader className="bg-muted/40">
              <TableRow className="hover:bg-transparent">
                <TableHead scope="col" className={headClass}>
                  Channel
                </TableHead>
                {SLACK_PERMISSION_FLAGS.map((flag) => (
                  <TableHead key={flag.attribute} scope="col" className={headClass}>
                    {flag.label}
                  </TableHead>
                ))}
                <TableHead scope="col" className={headClass}>
                  Delete
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {draft.map((row) => {
                const apiManaged = managesDms && isDirectMessage(row.channel_id);
                const name = channelNames[row.channel_id];
                return (
                  <TableRow key={row.channel_id}>
                    <TableCell className="min-w-64 px-5 py-3">
                      <div className="text-sm font-medium">
                        {apiManaged ? `DM ${directMessageOwnerName || row.channel_id}` : channelLabel(row.channel_id)}
                      </div>
                      {(apiManaged || name) && (
                        <div className="mt-0.5 font-mono text-xs text-muted-foreground">{row.channel_id}</div>
                      )}
                    </TableCell>
                    {SLACK_PERMISSION_FLAGS.map((flag) => (
                      <TableCell key={flag.attribute} className="px-5 py-3">
                        <Checkbox
                          aria-label={`${flag.label} for ${row.channel_id}`}
                          checked={row[flag.attribute]}
                          disabled={apiManaged}
                          onCheckedChange={(checked) => setFlag(row.channel_id, flag.attribute, checked === true)}
                        />
                      </TableCell>
                    ))}
                    <TableCell className="px-5 py-3">
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
                              className="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                            >
                              <Trash2 />
                            </Button>
                          }
                        />
                      )}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        ) : (
          <div className="p-4">
            <EmptyState className="py-6">No Slack channels selected.</EmptyState>
          </div>
        )}

        <div className="grid gap-4 border-t p-5 lg:grid-cols-[minmax(0,1fr)_auto_auto_auto] lg:items-end">
          <div>
            <Label className="mb-2 block" htmlFor="slack-channel-add">
              Add Channel
            </Label>
            <SlackChannelPicker
              key={pickerKey}
              id="slack-channel-add"
              loadOptions={loadOptions}
              value={newChannel}
              onChange={setNewChannel}
              onValidityChange={setPickerValid}
            />
          </div>
          <div className="flex flex-wrap gap-x-5 gap-y-3 lg:contents">
            {SLACK_PERMISSION_FLAGS.map((flag) => (
              <label key={flag.attribute} className="flex h-9 cursor-pointer items-center gap-2 lg:mb-6">
                <Checkbox
                  checked={newFlags[flag.attribute]}
                  aria-label={`${flag.label} for new channel`}
                  onCheckedChange={(checked) => setNewFlags((flags) => ({ ...flags, [flag.attribute]: checked === true }))}
                />
                <span className="text-sm font-medium">{flag.label}</span>
              </label>
            ))}
          </div>
        </div>
        {error && (
          <p role="alert" className="px-5 pb-3 text-sm text-destructive">
            {error}
          </p>
        )}
        <div className="px-5 pb-5">
          <Button type="button" disabled={!pickerValid || saving} onClick={() => void save()}>
            Save Slack channel permissions
          </Button>
        </div>
      </Card>

      {inherited.length > 0 && (
        <>
          <div className="mt-6 mb-3">
            <h3 className="text-sm font-semibold">Inherited From Roles</h3>
          </div>
          <Card className="gap-0 overflow-hidden py-0">
            <Table>
              <TableHeader className="bg-muted/40">
                <TableRow className="hover:bg-transparent">
                  <TableHead scope="col" className={headClass}>
                    Channel
                  </TableHead>
                  {SLACK_PERMISSION_FLAGS.map((flag) => (
                    <TableHead key={flag.attribute} scope="col" className={headClass}>
                      {flag.label}
                    </TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {inherited.map((row) => (
                  <TableRow key={row.channel_id}>
                    <TableCell className="px-5 py-3">
                      <div className="text-sm font-medium">{channelLabel(row.channel_id)}</div>
                      <div className="mt-0.5 font-mono text-xs text-muted-foreground">{row.channel_id}</div>
                    </TableCell>
                    {SLACK_PERMISSION_FLAGS.map((flag) => (
                      <TableCell key={flag.attribute} className="px-5 py-3">
                        <Checkbox
                          checked={row[flag.attribute]}
                          disabled
                          aria-label={`${flag.label} for ${row.channel_id} (inherited)`}
                        />
                      </TableCell>
                    ))}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </Card>
        </>
      )}
    </section>
  );
}
