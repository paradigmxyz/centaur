import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { LocalTime } from "@/components/local-time";

describe("LocalTime", () => {
  it("renders a time element with an ISO datetime and absolute title", () => {
    render(<LocalTime value="2026-06-04T18:30:00Z" />);
    const time = screen.getByRole("time");
    expect(time).toHaveAttribute("datetime", "2026-06-04T18:30:00.000Z");
    expect(time.getAttribute("title")).toMatch(/^\d{2}\/\d{2}\/2026 \d{2}:\d{2}:\d{2}$/);
  });

  it("renders a placeholder for null", () => {
    render(<LocalTime value={null} />);
    expect(screen.getByText("—")).toBeInTheDocument();
  });
});
