import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { BackLink } from "@/components/back-link";
import { LocalTime } from "@/components/local-time";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import { archiveSkill, setSkillShared, userLabel, useSkill, type Skill } from "../api";
import { skillsTabPath, toSentence, validationMessages } from "../sections/helpers";
import { SkillMarkdown, VisibilityBadge } from "../sections/SkillParts";

export default function SkillDetailPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.skills}>
      <SkillDetail />
    </RequireAccess>
  );
}

function SkillDetail() {
  const { id = "" } = useParams();
  const { data: skill } = useSkill(id);
  useDocumentTitle(skill.name);
  const { viewer } = skill;

  return (
    <>
      <nav className="mb-4" aria-label="Breadcrumb">
        <BackLink to={skillsTabPath(viewer.can_edit ? "mine" : "directory")}>Back to Skills</BackLink>
      </nav>

      <div className="mb-8">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div className="flex items-center gap-2">
            <h1 className="text-2xl font-semibold tracking-tight">{skill.name}</h1>
            <VisibilityBadge visibility={skill.visibility} />
          </div>
          <SkillActions skill={skill} />
        </div>
        <p className="mt-2 text-sm text-muted-foreground">
          by {skill.author} · updated <LocalTime value={skill.updated_at} format="relative" />
        </p>
      </div>

      <div className="max-w-3xl space-y-4">
        <section aria-labelledby="skill-description-heading" className="rounded-xl border bg-card p-5 shadow-xs">
          <h2 id="skill-description-heading" className="mb-3 text-sm font-semibold">
            Description
          </h2>
          <p className="text-sm leading-6 text-muted-foreground">{skill.description}</p>
        </section>

        <section aria-labelledby="skill-instructions-heading" className="rounded-xl border bg-card p-5 shadow-xs">
          <h2 id="skill-instructions-heading" className="mb-3 text-sm font-semibold">
            Instructions
          </h2>
          <SkillMarkdown html={skill.content_html} />
        </section>

        {skill.editors.length > 0 && (
          <section aria-labelledby="skill-editors-heading" className="rounded-xl border bg-card p-5 shadow-xs">
            <h2 id="skill-editors-heading" className="mb-3 text-sm font-semibold">
              Editors
            </h2>
            <ul className="space-y-2 text-sm text-muted-foreground">
              {skill.editors.map((editor) => (
                <li key={editor.id}>{userLabel(editor)}</li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </>
  );
}

/** Edit (owner/editor); owner: visibility switch + Archive; moderating admin: Make Private. */
function SkillActions({ skill }: { skill: Skill }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { viewer } = skill;
  const shared = skill.visibility === "shared";
  const [busy, setBusy] = useState(false);

  const setVisibility = async (share: boolean) => {
    setBusy(true);
    try {
      await setSkillShared(queryClient, skill.id, share);
      toast.success(share ? "Skill is now shared." : "Skill is now private.");
    } catch (error) {
      toast.error(toSentence(validationMessages(error)));
    } finally {
      setBusy(false);
    }
  };

  const archive = async () => {
    setBusy(true);
    try {
      await archiveSkill(queryClient, skill.id);
      toast.success("Skill archived.");
      navigate("/console/skills/mine");
    } catch (error) {
      toast.error(toSentence(validationMessages(error)));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex min-h-9 shrink-0 flex-wrap items-center gap-2">
      {viewer.can_edit && (
        <Button asChild variant="secondary">
          <Link to={`/console/skills/${skill.id}/edit`}>Edit</Link>
        </Button>
      )}
      {viewer.owner ? (
        <>
          <button
            type="button"
            role="switch"
            aria-checked={shared}
            disabled={busy}
            title={shared ? "Make this skill private" : "Make this skill public"}
            onClick={() => setVisibility(!shared)}
            className="group inline-flex h-9 cursor-pointer items-center gap-2.5 rounded-md border border-input bg-transparent px-3 text-sm shadow-xs transition-[color,box-shadow] outline-none hover:bg-accent focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 dark:bg-input/30 dark:hover:bg-input/50"
          >
            <span className={shared ? "text-muted-foreground" : "font-medium"}>Private</span>
            {/* The track and thumb of the shadcn Switch. */}
            <span
              aria-hidden="true"
              className={cn(
                "relative inline-flex h-[1.15rem] w-8 shrink-0 items-center rounded-full transition-colors",
                shared ? "bg-primary" : "bg-input dark:bg-input/80",
              )}
            >
              <span
                className={cn(
                  "block size-4 rounded-full transition-transform",
                  shared ? "translate-x-[calc(100%-2px)] bg-background dark:bg-primary-foreground" : "translate-x-0 bg-background dark:bg-foreground",
                )}
              />
            </span>
            <span className={shared ? "font-medium text-primary" : "text-muted-foreground"}>Public</span>
          </button>
          <Button variant="destructive" disabled={busy} onClick={archive}>
            Archive
          </Button>
        </>
      ) : (
        viewer.can_unshare && (
          <Button variant="destructive" disabled={busy} onClick={() => setVisibility(false)}>
            Make Private
          </Button>
        )
      )}
    </div>
  );
}
