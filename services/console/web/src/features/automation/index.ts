import { Clock, Workflow } from "lucide-react";

import type { FeatureModule } from "@/app/types";

const page = (load: () => Promise<{ default: React.ComponentType }>) => () =>
  load().then((m) => ({ Component: m.default }));

const automation: FeatureModule = {
  id: "automation",
  routes: [
    { path: "/console/scheduled_tasks", lazy: page(() => import("./pages/ScheduledTasksList")) },
    { path: "/console/scheduled_tasks/new", lazy: page(() => import("./pages/ScheduledTaskNew")) },
    { path: "/console/scheduled_tasks/:id/edit", lazy: page(() => import("./pages/ScheduledTaskEdit")) },
    { path: "/console/workflows", lazy: page(() => import("./pages/WorkflowsList")) },
    { path: "/console/workflows/:id", lazy: page(() => import("./pages/WorkflowDetail")) },
  ],
  nav: [
    {
      id: "scheduled_tasks",
      label: "Scheduled",
      to: "/console/scheduled_tasks",
      icon: Clock,
      placement: "group",
      order: 30,
      visible: (me) => me.nav.scheduled_tasks,
    },
    {
      id: "workflows",
      label: "Workflows",
      to: "/console/workflows",
      icon: Workflow,
      placement: "group",
      order: 40,
      visible: (me) => me.nav.workflows,
    },
  ],
};

export default automation;
