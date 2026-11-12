import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SlackChannelPicker } from "@/components/slack-channel-picker";

const noOptions = async () => ({ options: [] });

describe("SlackChannelPicker saved value", () => {
  it("shows a saved channel's label the way a fresh selection does", () => {
    render(
      <SlackChannelPicker
        aria-label="Delivery channel"
        loadOptions={noOptions}
        value="C0123456789"
        selectedLabel="#general"
        onChange={vi.fn()}
      />,
    );
    expect(screen.getByRole("combobox", { name: "Delivery channel" })).toHaveValue("#general (C0123456789)");
  });

  it("falls back to the raw id without a label", () => {
    render(<SlackChannelPicker aria-label="Delivery channel" loadOptions={noOptions} value="C0123456789" onChange={vi.fn()} />);
    expect(screen.getByRole("combobox", { name: "Delivery channel" })).toHaveValue("C0123456789");
  });
});
