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

/** A tab in the Control section strip (Principals, Roles, ...). */
export interface ControlTab {
  id: string;
  label: string;
  to: string;
  order: number;
  visible: (me: Me) => boolean;
  match?: string;
}

/**
 * What every web/src/features/<slice>/index.ts exports as default.
 *
 * - routes: rendered inside the authenticated app shell. Use absolute paths
 *   (e.g. "/console/roles/:id").
 * - publicRoutes: rendered without the shell or session (auth pages).
 * - nav / controlTabs: sidebar entries and Control tabs.
 */
export interface FeatureModule {
  id: string;
  routes?: RouteObject[];
  publicRoutes?: RouteObject[];
  nav?: NavEntry[];
  controlTabs?: ControlTab[];
}
