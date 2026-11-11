import { useMemo } from "react";
import { toast } from "sonner";

import { adminSlackChannelLoader, useSlackChannelNames } from "@/components/slack-channel-picker";
import { SlackChannelPermissionsEditor } from "@/components/slack-channel-permissions-editor";
import { errorMessage } from "@/lib/api";

import type { SectionProps } from "./types";

/** Role-owned Slack channel permissions: no DM handling, no inherited table. */
export function RoleSlackPermissionsSection({ role, mutations }: SectionProps) {
  const ids = useMemo(() => role.slack_channel_permissions.map((row) => row.channel_id), [role]);
  const catalog = useSlackChannelNames(ids);
  const loadOptions = useMemo(() => adminSlackChannelLoader(`role:${role.id}`), [role.id]);

  return (
    <SlackChannelPermissionsEditor
      permissions={role.slack_channel_permissions}
      channelNames={catalog.data?.names}
      catalogError={catalog.data?.catalogError}
      loadOptions={loadOptions}
      onSave={async (rows) => {
        try {
          await mutations.replaceSlackPermissions.mutateAsync(rows);
          toast.success("Updated Slack channel permissions.");
        } catch (error) {
          throw new Error(errorMessage(error), { cause: error });
        }
      }}
      onDelete={async (channelId) => {
        try {
          await mutations.deleteSlackPermission.mutateAsync(channelId);
          toast.success("Deleted Slack channel permission.");
        } catch (error) {
          toast.error(errorMessage(error));
        }
      }}
    />
  );
}
