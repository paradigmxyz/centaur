import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useForm } from "react-hook-form";
import { describe, expect, it, vi } from "vitest";

import { omitBlankSecrets, SecretField } from "@/components/form/secret-field";

function Harness({ onSubmit, stored }: { onSubmit: (values: Record<string, unknown>) => void; stored: boolean }) {
  const form = useForm<{ name: string; client_secret: string }>({ defaultValues: { name: "app", client_secret: "" } });
  return (
    <form onSubmit={form.handleSubmit((values) => onSubmit(omitBlankSecrets(values, ["client_secret"])))}>
      <SecretField control={form.control} name="client_secret" label="Client secret" hasStoredValue={stored} />
      <button type="submit">Save</button>
    </form>
  );
}

describe("SecretField", () => {
  it("is never prefilled and omitting it keeps the stored value", async () => {
    const onSubmit = vi.fn();
    render(<Harness onSubmit={onSubmit} stored />);
    const input = screen.getByLabelText("Client secret");
    expect(input).toHaveValue("");
    expect(input).toHaveAttribute("placeholder", "•••••••• (unchanged)");
    expect(input).toHaveAttribute("type", "password");

    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(onSubmit).toHaveBeenCalledWith({ name: "app" });
  });

  it("sends a newly typed value", async () => {
    const onSubmit = vi.fn();
    render(<Harness onSubmit={onSubmit} stored={false} />);
    await userEvent.type(screen.getByLabelText("Client secret"), "s3cret");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(onSubmit).toHaveBeenCalledWith({ name: "app", client_secret: "s3cret" });
  });
});
