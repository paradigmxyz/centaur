import { useQueryClient } from "@tanstack/react-query";
import { CircleCheck } from "lucide-react";
import { Link, Navigate, useNavigate } from "react-router";
import { toast } from "sonner";

import { AppLink } from "@/components/app-link";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { FormSection } from "@/components/form/fields";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage } from "@/lib/api";

import { deleteSshKey, usePersonalSshKey, type PersonalSshKey } from "../api";
import { formatUtcMinute, INTEGRATIONS_PATH } from "../sections/helpers";

// Any active user manages their own key; the API scopes it to the caller.
export default function SshKeyShowPage() {
  const { data: key } = usePersonalSshKey();
  if (!key.saved) return <Navigate to="/console/ssh_key/edit" replace />;
  return <SshKeyShow sshKey={key} />;
}

function SshKeyShow({ sshKey: key }: { sshKey: PersonalSshKey }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const onDelete = async () => {
    try {
      await deleteSshKey(queryClient);
      toast.success("SSH key deleted. Remove its public key from your environments if it is no longer needed.");
      navigate(INTEGRATIONS_PATH);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  return (
    <>
      <PageHeader title={key.name} documentTitle="SSH key" subtitle="Your personal SSH key" />
      <div className="max-w-3xl space-y-6">
        <div role="status" className="relative rounded-xl border border-success/30 bg-success/10 py-4 pr-4 pl-11">
          <CircleCheck className="absolute top-4.5 left-4 size-4 text-success" aria-hidden="true" />
          <p className="font-semibold text-success">SSH key saved</p>
          <p className="mt-1 text-sm text-foreground">
            Saved {formatUtcMinute(key.updated_at)}. Your private key is stored encrypted. You can leave this page.
          </p>
          <p className="mt-1 text-xs text-muted-foreground">
            This confirms the key was saved; it does not test access to an environment. Register the public key below
            in your environments.
          </p>
        </div>

        <FormSection title="Key details">
          <p className="mb-4 text-xs text-muted-foreground">Provide the destination URL when asking Centaur to connect.</p>
          <dl className="space-y-4 text-sm">
            <div>
              <dt className="mb-1 text-sm font-medium">Your key fingerprint</dt>
              <dd className="font-mono text-[13px] break-all text-foreground">{key.fingerprint}</dd>
            </div>
          </dl>
        </FormSection>

        <FormSection title="Granted to">
          <p className="mb-4 text-xs text-muted-foreground">
            Only these principals can use this key. Your Console and DM principals are granted automatically; anything
            else was granted by an administrator.
          </p>
          {key.grants.length > 0 ? (
            <ul className="space-y-1 text-sm">
              {key.grants.map((grant) => (
                <li key={grant.id} className="font-mono text-[13px] text-foreground">
                  {grant.grantee_type === "role" ? "Role" : "Principal"}: {grant.label}
                  {grant.principal_kind && <span className="text-muted-foreground"> ({grant.principal_kind})</span>}
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-muted-foreground">
              Not granted yet. It is granted to your Console principal when you next use Console, and to your Slack DM
              once a verified Slack identity is linked.
            </p>
          )}
        </FormSection>

        <FormSection title="Register your public key in your environments">
          <p className="mb-4 text-xs text-muted-foreground">
            Add this public key to each environment&apos;s SSH access settings. You can reuse this key for any URL allowed
            by your administrator. Saving does not register it automatically.
          </p>
          <label htmlFor="ssh-public-key" className="mb-2 block text-sm font-medium">
            Public key
          </label>
          <Textarea id="ssh-public-key" className="font-mono" rows={4} readOnly spellCheck={false} value={key.public_key ?? ""} />
          <p className="mt-1 text-xs text-muted-foreground">
            Your private key is stored encrypted and cannot be viewed or downloaded.
          </p>
        </FormSection>

        <div className="flex flex-wrap gap-3">
          <Button asChild>
            <Link to="/console/ssh_key/edit">Edit</Link>
          </Button>
          <ConfirmDialog
            title="Delete this SSH key?"
            description="Access ends within seconds."
            confirmLabel="Delete"
            onConfirm={onDelete}
            trigger={<Button variant="outline" className="text-destructive hover:text-destructive">Delete</Button>}
          />
          <Button asChild variant="ghost">
            <AppLink to={INTEGRATIONS_PATH}>Back to integrations</AppLink>
          </Button>
        </div>
      </div>
    </>
  );
}
