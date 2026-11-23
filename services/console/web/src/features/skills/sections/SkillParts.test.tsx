import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SkillMarkdown } from "./SkillParts";

describe("SkillMarkdown", () => {
  it("renders GFM and keeps only http(s) and mailto links", () => {
    const { container } = render(
      <SkillMarkdown
        markdown={[
          "# Title",
          "",
          "**bold** and *em*",
          "",
          "| a | b |",
          "| :- | -: |",
          "| 1 | 2 |",
          "",
          "[ok](https://example.test/doc)",
          "[mail](mailto:ops@example.test)",
          "[bad](javascript:alert(1))",
          "<div>raw html</div>",
        ].join("\n")}
      />,
    );

    expect(container.querySelector("h1")?.textContent).toBe("Title");
    expect(container.querySelector("strong")?.textContent).toBe("bold");
    expect(container.querySelector("em")?.textContent).toBe("em");
    expect(container.querySelector("table")).not.toBeNull();
    expect((container.querySelector("th") as HTMLElement).style.textAlign).toBe("left");
    expect((container.querySelector("td") as HTMLElement).style.textAlign).toBe("left");

    const link = (label: string) =>
      [...container.querySelectorAll("a")].find((anchor) => anchor.textContent === label);
    expect(link("ok")?.getAttribute("href")).toBe("https://example.test/doc");
    expect(link("ok")?.getAttribute("target")).toBe("_blank");
    expect(link("ok")?.getAttribute("rel")).toBe("noopener noreferrer");
    expect(link("ok")?.className).toBe("console-markdown-link");
    expect(link("mail")?.getAttribute("href")).toBe("mailto:ops@example.test");
    expect(link("bad")?.hasAttribute("href")).toBe(false);
    expect(container.innerHTML).not.toContain("<script");
    expect(container.textContent).not.toContain("raw html");
  });
});
