import type { LucideIcon } from "lucide-react";
import type { RouteObject } from "react-router";

import type { Me } from "@/app/session";

/** A sidebar entry. `primary` items sit in the top list, `group` items below. */
export interface NavEntry {
  id: string;
  label: string;
  to: string;
  icon: LucideIcon;
  placement: "primary" | "group";
  order: number;
  visible: (me: Me) => boolean;
  /** Path prefixes that mark the entry active (defaults to `to`). */
  match?: string[];
}

export interface ControlTab {
  id: string;
  label: string;
  to: string;
  order: number;
  visible: (me: Me) => boolean;
  match?: string;
}

/** `routes` render inside the authenticated shell; `publicRoutes` without shell or session. */
export interface FeatureModule {
  id: string;
  routes?: RouteObject[];
  publicRoutes?: RouteObject[];
  nav?: NavEntry[];
  controlTabs?: ControlTab[];
}
