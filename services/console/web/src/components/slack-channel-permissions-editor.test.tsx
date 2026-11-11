import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { SlackChannelPermissionsEditor } from "@/components/slack-channel-permissions-editor";

const rows = [
  { channel_id: "C0123456789", upload_enabled: true, download_enabled: false, history_enabled: true },
  { channel_id: "D0123456789", upload_enabled: false, download_enabled: true, history_enabled: false },
];

function setup(overrides: Partial<Parameters<typeof SlackChannelPermissionsEditor>[0]> = {}) {
  const onSave = vi.fn().mockResolvedValue(undefined);
  const onDelete = vi.fn().mockResolvedValue(undefined);
  const loadOptions = vi.fn().mockResolvedValue({
    options: [{ channel_id: "C1111111111", label: "#engineering", description: "C1111111111 · Public" }],
    error: null,
  });
  render(
    <SlackChannelPermissionsEditor
      permissions={rows}
      inherited={[{ channel_id: "G9876543210", upload_enabled: false, download_enabled: false, history_enabled: true }]}
      channelNames={{ C0123456789: "general", G9876543210: "private" }}
      directMessageOwnerName="Bob"
      loadOptions={loadOptions}
      onSave={onSave}
      onDelete={onDelete}
      {...overrides}
    />,
  );
  return { onSave, onDelete, loadOptions };
}

describe("SlackChannelPermissionsEditor", () => {
  it("renders catalog names, API-managed DM rows, and inherited rows", () => {
    setup();
    expect(screen.getByText("#general")).toBeInTheDocument();
    expect(screen.getByText("DM Bob")).toBeInTheDocument();
    expect(screen.getByText("API-managed")).toBeInTheDocument();
    expect(screen.getByText("Inherited From Roles")).toBeInTheDocument();
    expect(screen.getByText("#private")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Upload for D0123456789" })).toBeDisabled();
  });

  it("saves edited flags without the API-managed DM rows", async () => {
    const { onSave } = setup();
    await userEvent.click(screen.getByRole("checkbox", { name: "Download for C0123456789" }));
    await userEvent.click(screen.getByRole("button", { name: "Save Slack channel permissions" }));
    expect(onSave).toHaveBeenCalledWith([
      { channel_id: "C0123456789", upload_enabled: true, download_enabled: true, history_enabled: true },
    ]);
  });

  it("adds a picked channel with default flags", async () => {
    const { onSave } = setup();
    const input = screen.getByRole("combobox");
    await userEvent.type(input, "eng");
    const option = await screen.findByRole("option", { name: /#engineering/ });
    await userEvent.click(option);
    await userEvent.click(screen.getByRole("button", { name: "Save Slack channel permissions" }));
    expect(onSave).toHaveBeenCalledWith([
      expect.objectContaining({ channel_id: "C0123456789" }),
      { channel_id: "C1111111111", upload_enabled: true, download_enabled: true, history_enabled: true },
    ]);
  });

  it("disables save while typed text is not a channel id, and accepts a raw id", async () => {
    const { onSave } = setup();
    const input = screen.getByRole("combobox");
    const save = screen.getByRole("button", { name: "Save Slack channel permissions" });
    await userEvent.type(input, "not a channel");
    expect(save).toBeDisabled();
    await userEvent.clear(input);
    await userEvent.type(input, "c2222222222");
    expect(save).toBeEnabled();
    await userEvent.click(save);
    expect(onSave.mock.calls[0]?.[0]).toContainEqual(expect.objectContaining({ channel_id: "C2222222222" }));
  });

  it("rejects a duplicate channel client-side", async () => {
    const { onSave } = setup();
    await userEvent.type(screen.getByRole("combobox"), "C0123456789");
    await userEvent.click(screen.getByRole("button", { name: "Save Slack channel permissions" }));
    expect(onSave).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent("Each Slack channel can only be selected once.");
  });

  it("confirms before deleting a row", async () => {
    const { onDelete } = setup();
    await userEvent.click(screen.getByRole("button", { name: "Delete C0123456789" }));
    const dialog = await screen.findByRole("alertdialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    expect(onDelete).toHaveBeenCalledWith("C0123456789");
  });
});
