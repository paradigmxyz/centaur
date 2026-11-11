import { ShieldCheck } from "lucide-react";

import type { Me } from "@/app/session";
import type { ControlTab, FeatureModule, NavEntry } from "@/app/types";

export function visibleControlTabs(features: FeatureModule[], me: Me): ControlTab[] {
  return features
    .flatMap((feature) => feature.controlTabs ?? [])
    .filter((tab) => tab.visible(me))
    .sort((a, b) => a.order - b.order);
}

/**
 * Sidebar entries for the current user. The shell owns the "Control" entry,
 * which points at the first Control tab and is active anywhere in the section.
 */
export function visibleNav(features: FeatureModule[], me: Me): NavEntry[] {
  const entries = features.flatMap((feature) => feature.nav ?? []).filter((entry) => entry.visible(me));
  const tabs = visibleControlTabs(features, me);
  const first = tabs[0];
  if (me.nav.control && first) {
    entries.push({
      id: "control",
      label: "Control",
      to: first.to,
      icon: ShieldCheck,
      placement: "primary",
      order: 0,
      visible: () => true,
      match: tabs.map((tab) => tab.match ?? tab.to),
    });
  }
  return entries.sort((a, b) => a.order - b.order);
}

export function isActivePath(pathname: string, prefixes: string[]): boolean {
  return prefixes.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}
