// Skills slice.
//
// Stub until this slice moves to the SPA. Nav entries and Control tabs below
// point at the legacy Rails pages; once the slice registers SPA routes for the
// same paths, the shell links to them client-side automatically.
import { Workflow } from "lucide-react";

import type { FeatureModule } from "@/app/types";

const skills: FeatureModule = {
  id: "skills",
  routes: [],
  nav: [
    {
      id: "skills",
      label: "Skills",
      to: "/console/skills",
      icon: Workflow,
      placement: "group",
      order: 20,
      visible: (me) => me.nav.skills,
    },
  ],
};

export default skills;
