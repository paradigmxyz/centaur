// Data access for the Skills pages and the personal SSH key pages. Query keys
// live under "skills" and "ssh-key" (see lib/query-keys.ts).
import { keepPreviousData, useMutation, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";

import { api, apiRequest, type DataEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";

export const skillKeys = createQueryKeys("skills");
export const sshKeyKeys = createQueryKeys("ssh-key");

export type SkillTab = "directory" | "mine";
export type SkillVisibility = "private" | "shared";

export interface SkillUser {
  id: string;
  name: string | null;
  email: string;
}

export interface SkillSummary {
  id: string;
  name: string;
  description: string;
  visibility: SkillVisibility;
  author: string;
  owner: SkillUser;
  updated_at: string | null;
  viewer: { owner: boolean };
}

export interface Skill extends Omit<SkillSummary, "viewer"> {
  instructions: string;
  /** Sanitized server-rendered Markdown (safe for innerHTML). */
  content_html: string;
  lock_version: number;
  checksum: string;
  created_at: string | null;
  editors: SkillUser[];
  viewer: { owner: boolean; can_edit: boolean; can_manage: boolean; can_unshare: boolean };
}

export interface SkillInput {
  name: string;
  description: string;
  instructions: string;
  lock_version?: number;
  /** Omit to keep the current editors (only owners may send it). */
  editor_ids?: string[];
}

/** "Name (email)" or just the email, as the legacy pages labelled users. */
export function userLabel(user: SkillUser): string {
  return user.name ? `${user.name} (${user.email})` : user.email;
}

const base = "/api/v1/skills";

export function useSkillList(params: { tab: SkillTab; q: string }) {
  return useQuery({
    queryKey: skillKeys.list(params),
    queryFn: ({ signal }) => api.get<DataEnvelope<SkillSummary[]>>(base, { tab: params.tab, q: params.q }, signal),
    select: (body) => body.data,
    placeholderData: keepPreviousData,
  });
}

export function useSkill(id: string) {
  return useSuspenseQuery({
    queryKey: skillKeys.detail(id),
    queryFn: ({ signal }) => api.get<DataEnvelope<Skill>>(`${base}/${id}`, undefined, signal),
    select: (body) => body.data,
  });
}

export function useEditorCandidates(enabled: boolean) {
  return useQuery({
    queryKey: [...skillKeys.all, "editor_candidates"],
    queryFn: ({ signal }) => api.get<DataEnvelope<SkillUser[]>>(`${base}/editor_candidates`, undefined, signal),
    select: (body) => body.data,
    enabled,
  });
}

export function useCreateSkill() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: SkillInput) => api.post<DataEnvelope<Skill>>(base, { data }),
    onSuccess: (body) => {
      queryClient.setQueryData(skillKeys.detail(body.data.id), body);
      return queryClient.invalidateQueries({ queryKey: skillKeys.lists() });
    },
  });
}

/** Mutations for one skill; each stores the returned skill and refreshes the lists. */
export function useSkillMutations(id: string) {
  const queryClient = useQueryClient();
  const store = (body: DataEnvelope<Skill>) => {
    queryClient.setQueryData(skillKeys.detail(id), body);
    return queryClient.invalidateQueries({ queryKey: skillKeys.lists() });
  };

  return {
    update: useMutation({
      mutationFn: (data: SkillInput) => api.patch<DataEnvelope<Skill>>(`${base}/${id}`, { data }),
      onSuccess: store,
    }),
    share: useMutation({
      mutationFn: () => api.post<DataEnvelope<Skill>>(`${base}/${id}/share`),
      onSuccess: store,
    }),
    unshare: useMutation({
      mutationFn: () => api.post<DataEnvelope<Skill>>(`${base}/${id}/unshare`),
      onSuccess: store,
    }),
    archive: useMutation({
      mutationFn: () => api.delete(`${base}/${id}`),
      // Lists only: dropping the detail query while its page is mounted would
      // refetch the archived skill (a 404).
      onSuccess: () => queryClient.invalidateQueries({ queryKey: skillKeys.lists() }),
    }),
  };
}

// -- personal SSH key -------------------------------------------------------

export interface SshKeyGrant {
  id: string;
  grantee_type: "role" | "principal";
  grantee_id: string;
  label: string;
  principal_kind: string | null;
}

/** The caller's personal SSH key. Never includes the private key. */
export interface PersonalSshKey {
  id: string | null;
  saved: boolean;
  name: string;
  has_private_key: boolean;
  fingerprint: string | null;
  public_key: string | null;
  created_at: string | null;
  updated_at: string | null;
  grants: SshKeyGrant[];
}

export interface SshKeyValidation {
  valid: true;
  message: string;
  fingerprint: string;
}

const sshKeyPath = "/api/v1/me/ssh_key";

export function usePersonalSshKey() {
  return useSuspenseQuery({
    queryKey: sshKeyKeys.all,
    queryFn: ({ signal }) => api.get<DataEnvelope<PersonalSshKey>>(sshKeyPath, undefined, signal),
    select: (body) => body.data,
    staleTime: 0,
  });
}

/** Checks a pasted private key without saving it (422 ApiError when invalid). */
export function validateSshKey(privateKey: string, signal: AbortSignal) {
  return apiRequest<DataEnvelope<SshKeyValidation>>(`${sshKeyPath}/validate`, {
    method: "POST",
    body: { data: { private_key: privateKey } },
    signal,
  });
}

export function useSshKeyMutations() {
  const queryClient = useQueryClient();
  return {
    save: useMutation({
      mutationFn: (data: { name: string; private_key?: string }) =>
        api.put<DataEnvelope<PersonalSshKey>>(sshKeyPath, { data }),
      onSuccess: (body) => queryClient.setQueryData(sshKeyKeys.all, body),
    }),
    destroy: useMutation({
      mutationFn: () => api.delete(sshKeyPath),
      // Mark stale without refetching: the mounted page navigates away, and
      // the next visit loads the (now unsaved) key.
      onSuccess: () => queryClient.invalidateQueries({ queryKey: sshKeyKeys.all, refetchType: "none" }),
    }),
  };
}
