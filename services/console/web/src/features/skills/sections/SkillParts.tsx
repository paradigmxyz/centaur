import { Link, useLocation } from "react-router";

import { ResourceBadge, RolePill } from "@/components/badges";
import { cn } from "@/lib/utils";

import type { SkillTab, SkillVisibility } from "../api";
import { skillsTabPath } from "./helpers";

export function VisibilityBadge({ visibility }: { visibility: SkillVisibility }) {
  return visibility === "shared" ? <ResourceBadge>PUBLIC</ResourceBadge> : <RolePill>PRIVATE</RolePill>;
}

export function SkillTabs({ tab, q }: { tab: SkillTab; q: string }) {
  const { pathname } = useLocation();
  const tabs: { id: SkillTab; label: string }[] = [
    { id: "directory", label: "Public Skills" },
    { id: "mine", label: "My Skills" },
  ];
  return (
    <nav
      aria-label="Skill sections"
      className="mb-6 inline-flex h-9 w-max max-w-full items-center overflow-x-auto rounded-lg bg-muted p-[3px] text-muted-foreground"
    >
      {tabs.map((item) => {
        const path = skillsTabPath(item.id);
        const active = item.id === tab || pathname === path;
        return (
          <Link
            key={item.id}
            to={q ? `${path}?q=${encodeURIComponent(q)}` : path}
            aria-current={active ? "page" : undefined}
            className={cn(
              "inline-flex h-full flex-none items-center rounded-md border border-transparent px-3 text-sm font-medium whitespace-nowrap transition-[color,box-shadow] outline-none hover:text-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
              active && "bg-background text-foreground shadow-sm dark:border-input dark:bg-input/30",
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
  "[&_h1]:mt-5 [&_h1]:mb-2 [&_h1]:text-lg [&_h1]:font-semibold [&_h1]:tracking-tight [&_h1]:text-foreground [&_h1:first-child]:mt-0",
  "[&_h2]:mt-5 [&_h2]:mb-2 [&_h2]:text-base [&_h2]:font-semibold [&_h2]:text-foreground [&_h2:first-child]:mt-0",
  "[&_h3]:mt-4 [&_h3]:mb-2 [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:text-foreground [&_h3:first-child]:mt-0",
  "[&_h4]:mt-4 [&_h4]:mb-2 [&_h4]:text-sm [&_h4]:font-semibold [&_h4]:text-foreground [&_h4:first-child]:mt-0",
  "[&_ul]:mb-3 [&_ul]:list-disc [&_ul]:space-y-1 [&_ul]:pl-5 [&_ol]:mb-3 [&_ol]:list-decimal [&_ol]:space-y-1 [&_ol]:pl-5",
  "[&_blockquote]:mb-3 [&_blockquote]:border-l [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground",
  "[&_pre]:mb-3 [&_pre]:overflow-x-auto [&_pre]:rounded-lg [&_pre]:border [&_pre]:bg-muted/40 [&_pre]:p-3 [&_pre]:font-mono [&_pre]:text-xs [&_pre]:leading-5",
  "[&_code]:rounded-md [&_code]:bg-muted [&_code]:font-mono [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-[0.92em] [&_pre_code]:bg-transparent [&_pre_code]:p-0",
  "[&_strong]:font-semibold [&_strong]:text-foreground [&_em]:italic [&_del]:line-through",
  "[&_div]:mb-3 [&_div]:overflow-x-auto [&_table]:w-full [&_table]:border-collapse [&_table]:text-sm",
  "[&_th]:border-b [&_th]:border-border [&_th]:px-3 [&_th]:py-1.5 [&_th]:font-semibold [&_th]:text-foreground",
  "[&_td]:border-b [&_td]:border-border/70 [&_td]:px-3 [&_td]:py-1.5 [&_td]:align-top",
  "[&_.text-left]:text-left [&_.text-center]:text-center [&_.text-right]:text-right",
  "[&_a]:text-primary [&_a]:underline [&_a]:decoration-primary/30 [&_a]:underline-offset-2 [&_a:hover]:decoration-primary",
].join(" ");

/**
 * Skill instructions. `html` comes from the API's `content_html`, which the
 * server builds from escaped text and passes through an allow-list sanitizer
 * (no scripts, attributes limited to class/href/rel/target, http(s) links only).
 */
export function SkillMarkdown({ html }: { html: string }) {
  return <article className={MARKDOWN_CLASSES} dangerouslySetInnerHTML={{ __html: html }} />;
}
