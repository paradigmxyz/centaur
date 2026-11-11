// Skills slice: skill authoring (Public Skills / My Skills, show, new, edit)
// and the caller's personal SSH key (/console/ssh_key), both open to every
// active user. The Integrations page links to the SSH key pages.
import { Workflow } from "lucide-react";
import type { ComponentType } from "react";

import type { FeatureModule } from "@/app/types";

const page = (load: () => Promise<{ default: ComponentType }>) => () =>
  load().then((m) => ({ Component: m.default }));

const skills: FeatureModule = {
  id: "skills",
  routes: [
    { path: "/console/skills", lazy: page(() => import("./pages/SkillsList")) },
    { path: "/console/skills/mine", lazy: page(() => import("./pages/MySkillsList")) },
    { path: "/console/skills/new", lazy: page(() => import("./pages/SkillNew")) },
    { path: "/console/skills/:id", lazy: page(() => import("./pages/SkillDetail")) },
    { path: "/console/skills/:id/edit", lazy: page(() => import("./pages/SkillEdit")) },
    { path: "/console/ssh_key", lazy: page(() => import("./pages/SshKeyShow")) },
    { path: "/console/ssh_key/edit", lazy: page(() => import("./pages/SshKeyEdit")) },
  ],
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
