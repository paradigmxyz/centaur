// Small presentational pieces shared by the skills pages.
import { Link, useLocation } from "react-router";

import { ResourceBadge, RolePill } from "@/components/badges";
import { cn } from "@/lib/utils";

import type { SkillTab, SkillVisibility } from "../api";
import { skillsTabPath } from "./helpers";

/** PUBLIC (green) / PRIVATE (role pill), as on the legacy pages. */
export function VisibilityBadge({ visibility }: { visibility: SkillVisibility }) {
  return visibility === "shared" ? <ResourceBadge>PUBLIC</ResourceBadge> : <RolePill>PRIVATE</RolePill>;
}

/** "Public Skills" / "My Skills" tab strip; the search query carries across tabs. */
export function SkillTabs({ tab, q }: { tab: SkillTab; q: string }) {
  const { pathname } = useLocation();
  const tabs: { id: SkillTab; label: string }[] = [
    { id: "directory", label: "Public Skills" },
    { id: "mine", label: "My Skills" },
  ];
  return (
    <nav aria-label="Skill sections" className="mb-6 flex gap-1 overflow-x-auto border-b border-border">
      {tabs.map((item) => {
        const path = skillsTabPath(item.id);
        const active = item.id === tab || pathname === path;
        return (
          <Link
            key={item.id}
            to={q ? `${path}?q=${encodeURIComponent(q)}` : path}
            aria-current={active ? "page" : undefined}
            className={cn(
              "-mb-px flex-none border-b border-transparent px-3 py-2.5 text-sm font-medium whitespace-nowrap text-zinc-400 transition-colors hover:text-heading light:text-[#6b7078]",
              active && "border-primary text-centaur-400 light:text-primary",
            )}
          >
            {item.label}
          </Link>
        );
      })}
    </nav>
  );
}

// Styles for SkillMarkdown's output (app/services/skill_markdown.rb), which
// emits bare semantic HTML plus `console-markdown-link` and the table
// alignment classes (text-left / text-center / text-right).
const MARKDOWN_CLASSES = [
  "console-markdown text-sm leading-6 text-foreground",
  "[&_p]:mb-3 [&_p:last-child]:mb-0",
  "[&_h1]:mt-4 [&_h1]:mb-2 [&_h1]:text-sm [&_h1]:font-semibold [&_h1]:text-heading [&_h1:first-child]:mt-0",
  "[&_h2]:mt-4 [&_h2]:mb-2 [&_h2]:text-sm [&_h2]:font-semibold [&_h2]:text-heading [&_h2:first-child]:mt-0",
  "[&_h3]:mt-4 [&_h3]:mb-2 [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:text-heading [&_h3:first-child]:mt-0",
  "[&_h4]:mt-4 [&_h4]:mb-2 [&_h4]:text-sm [&_h4]:font-semibold [&_h4]:text-heading [&_h4:first-child]:mt-0",
  "[&_ul]:mb-3 [&_ul]:list-disc [&_ul]:space-y-1 [&_ul]:pl-5 [&_ol]:mb-3 [&_ol]:list-decimal [&_ol]:space-y-1 [&_ol]:pl-5",
  "[&_blockquote]:mb-3 [&_blockquote]:border-l [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground",
  "[&_pre]:mb-3 [&_pre]:overflow-x-auto [&_pre]:rounded-sm [&_pre]:bg-muted [&_pre]:p-3 [&_pre]:text-xs [&_pre]:leading-5",
  "[&_code]:rounded-sm [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-[0.92em] [&_pre_code]:bg-transparent [&_pre_code]:p-0",
  "[&_strong]:font-semibold [&_strong]:text-heading [&_em]:italic [&_del]:line-through",
  "[&_div]:mb-3 [&_div]:overflow-x-auto [&_table]:w-full [&_table]:border-collapse [&_table]:text-sm",
  "[&_th]:border-b [&_th]:border-border [&_th]:px-3 [&_th]:py-1.5 [&_th]:font-semibold [&_th]:text-heading",
  "[&_td]:border-b [&_td]:border-border/70 [&_td]:px-3 [&_td]:py-1.5 [&_td]:align-top",
  "[&_.text-left]:text-left [&_.text-center]:text-center [&_.text-right]:text-right",
  "[&_a]:text-sky-300 [&_a]:underline [&_a]:decoration-sky-400/30 [&_a]:underline-offset-2 [&_a:hover]:text-sky-200 light:[&_a]:text-sky-700",
].join(" ");

/**
 * Skill instructions. `html` comes from the API's `content_html`, which the
 * server builds from escaped text and passes through an allow-list sanitizer
 * (no scripts, attributes limited to class/href/rel/target, http(s) links only).
 */
export function SkillMarkdown({ html }: { html: string }) {
  return <article className={MARKDOWN_CLASSES} dangerouslySetInnerHTML={{ __html: html }} />;
}
