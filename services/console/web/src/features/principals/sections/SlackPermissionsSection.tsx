import { useQueryClient } from "@tanstack/react-query";
import { useMemo } from "react";
import { toast } from "sonner";

import { adminSlackChannelLoader, useSlackChannelNames } from "@/components/slack-channel-picker";
import { SlackChannelPermissionsEditor } from "@/components/slack-channel-permissions-editor";
import { errorMessage } from "@/lib/api";

import { deletePrincipalSlackPermission, replacePrincipalSlackPermissions } from "../api";
import type { SectionProps } from "./types";

export function SlackPermissionsSection({ principal }: SectionProps) {
  const queryClient = useQueryClient();
  const ids = useMemo(
    () => [
      ...principal.slack_channel_permissions.map((row) => row.channel_id),
      ...principal.inherited_slack_channel_permissions.map((row) => row.channel_id),
    ],
    [principal],
  );
  const catalog = useSlackChannelNames(ids);
  const loadOptions = useMemo(() => adminSlackChannelLoader(`principal:${principal.id}`), [principal.id]);

  return (
    <SlackChannelPermissionsEditor
      permissions={principal.slack_channel_permissions}
      inherited={principal.inherited_slack_channel_permissions}
      channelNames={catalog.data?.names}
      catalogError={
        catalog.isError
          ? `Could not load Slack channel names: ${errorMessage(catalog.error)}`
          : catalog.data?.catalogError
      }
      directMessageOwnerName={principal.name || principal.slack_user_id || ""}
      loadOptions={loadOptions}
      onSave={async (rows) => {
        try {
          await replacePrincipalSlackPermissions(queryClient, principal.id, rows);
          toast.success("Updated Slack channel permissions.");
        } catch (error) {
          throw new Error(errorMessage(error), { cause: error });
        }
      }}
      onDelete={async (channelId) => {
        try {
          await deletePrincipalSlackPermission(queryClient, principal.id, channelId);
          toast.success("Deleted Slack channel permission.");
        } catch (error) {
          toast.error(errorMessage(error));
        }
      }}
    />
  );
}
