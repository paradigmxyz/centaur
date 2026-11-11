// Automation slice (scheduled tasks and workflows).
//
// Stub until this slice moves to the SPA. Nav entries and Control tabs below
// point at the legacy Rails pages; once the slice registers SPA routes for the
// same paths, the shell links to them client-side automatically.
import { Clock, Workflow } from "lucide-react";

import type { FeatureModule } from "@/app/types";

const automation: FeatureModule = {
  id: "automation",
  routes: [],
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
