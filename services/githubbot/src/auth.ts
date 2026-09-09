export type GithubbotAuthInput = {
  appId?: string;
  installationId?: number;
  privateKey?: string;
  token?: string;
};

export type GithubbotAuth =
  | { token: string }
  | { appId: string; installationId: number; privateKey: string };

/** Resolve exactly one explicit GitHub authentication mode at startup. */
export function resolveGithubbotAuth(input: GithubbotAuthInput): GithubbotAuth {
  const token = input.token?.trim();
  const appId = input.appId?.trim();
  const privateKey = input.privateKey?.trim();
  const hasAnyAppField =
    Boolean(appId || privateKey) || input.installationId !== undefined;

  if (token && hasAnyAppField) {
    throw new Error("Configure either GITHUB_TOKEN or GitHub App credentials, not both");
  }

  if (token) return { token };

  if (!appId || input.installationId === undefined || !privateKey) {
    throw new Error(
      "GitHub authentication requires GITHUB_TOKEN or all of GITHUB_APP_ID, GITHUB_APP_INSTALLATION_ID, and GITHUB_APP_PRIVATE_KEY",
    );
  }

  if (!Number.isSafeInteger(input.installationId) || input.installationId <= 0) {
    throw new Error("GITHUB_APP_INSTALLATION_ID must be a positive integer");
  }

  return { appId, installationId: input.installationId, privateKey };
}
