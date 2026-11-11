import type { Principal, usePrincipalMutations } from "../api";

export interface SectionProps {
  principal: Principal;
  mutations: ReturnType<typeof usePrincipalMutations>;
}
