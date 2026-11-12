/** Ruby's String#capitalize for provider names ("google" -> "Google"). */
export function providerLabel(provider: string): string {
  return provider.charAt(0).toUpperCase() + provider.slice(1).toLowerCase();
}

export function grantLabel(grant: string): string {
  return grant
    .split("_")
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(" ");
}
