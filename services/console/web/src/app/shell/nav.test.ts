import { Link2 } from "lucide-react";
import { describe, expect, it } from "vitest";

import { isActivePath, visibleControlTabs, visibleNav } from "@/app/shell/nav";
import type { FeatureModule } from "@/app/types";
import { adminMe, operatorMe } from "@/test/utils";

const features: FeatureModule[] = [
  {
    id: "a",
    controlTabs: [
      { id: "roles", label: "Roles", to: "/console/roles", order: 20, visible: (me) => me.nav.roles },
      { id: "principals", label: "Principals", to: "/console/principals", order: 10, visible: (me) => me.nav.principals },
    ],
    nav: [
      {
        id: "integrations",
        label: "Integrations",
        to: "/console/integrations",
        icon: Link2,
        placement: "primary",
        order: 10,
        visible: (me) => me.nav.integrations,
      },
    ],
  },
];

describe("shell navigation", () => {
  it("adds a Control entry for admins pointing at the first tab", () => {
    const nav = visibleNav(features, adminMe());
    expect(nav.map((entry) => entry.label)).toEqual(["Control", "Integrations"]);
    expect(nav[0]?.to).toBe("/console/principals");
    expect(nav[0]?.match).toEqual(["/console/principals", "/console/roles"]);
  });

  it("hides Control and admin tabs from operators", () => {
    const me = operatorMe({ nav: { ...operatorMe().nav, integrations: true } });
    expect(visibleNav(features, me).map((entry) => entry.label)).toEqual(["Integrations"]);
    expect(visibleControlTabs(features, me)).toEqual([]);
  });

  it("matches nested paths but not sibling prefixes", () => {
    expect(isActivePath("/console/roles/role_1", ["/console/roles"])).toBe(true);
    expect(isActivePath("/console/rolesx", ["/console/roles"])).toBe(false);
  });
});
