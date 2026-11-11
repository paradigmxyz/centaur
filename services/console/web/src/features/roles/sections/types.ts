import type { Role, useRoleMutations } from "../api";

export interface SectionProps {
  role: Role;
  mutations: ReturnType<typeof useRoleMutations>;
}
