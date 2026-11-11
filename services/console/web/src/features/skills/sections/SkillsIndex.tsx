// The Public Skills / My Skills list (legacy skills/index), shared by both tabs.
import { Link, useSearchParams } from "react-router";

import { useSession } from "@/app/session";
import { ErrorAlert } from "@/components/alerts";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { SearchBox } from "@/components/search-box";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";

import { useSkillList, type SkillSummary, type SkillTab } from "../api";
import { SkillTabs, VisibilityBadge } from "./SkillParts";

const COPY = {
  directory: {
    title: "Public Skills",
    subtitle: "Browse skills available to every agent.",
    placeholder: "Search public skills",
    empty: "No public skills yet.",
  },
  mine: {
    title: "My Skills",
    subtitle: "Create and manage your private and public skills.",
    placeholder: "Search my skills",
    empty: "You have not created any skills yet.",
  },
} as const;

export function SkillsIndexPage({ tab }: { tab: SkillTab }) {
  return (
    <RequireAccess allowed={(me) => me.nav.skills}>
      <SkillsIndex tab={tab} />
    </RequireAccess>
  );
}

function SkillsIndex({ tab }: { tab: SkillTab }) {
  const me = useSession();
  const [params] = useSearchParams();
  const q = (params.get("q") ?? "").trim();
  const list = useSkillList({ tab, q });
  const copy = COPY[tab];

  const columns: Column<SkillSummary>[] = [
    {
      id: "name",
      header: "Name",
      className: "w-[24%] overflow-hidden",
      cell: (skill) => (
        <Link
          to={`/console/skills/${skill.id}`}
          title={skill.name}
          className="block min-w-0 truncate font-medium text-primary hover:underline"
        >
          {skill.name}
        </Link>
      ),
    },
    {
      id: "description",
      header: "Description",
      className: "w-[46%] overflow-hidden text-sm text-muted-foreground",
      cell: (skill) => (
        <span className="block min-w-0 truncate" title={skill.description}>
          {skill.description}
        </span>
      ),
    },
    tab === "mine"
      ? {
          id: "visibility",
          header: "Visibility",
          className: "w-[15%] overflow-hidden",
          cell: (skill) => <VisibilityBadge visibility={skill.visibility} />,
        }
      : {
          id: "author",
          header: "Author",
          className: "w-[15%] overflow-hidden text-sm text-muted-foreground",
          cell: (skill) => (
            <span className="block min-w-0 truncate">{skill.owner.id === me.user.id ? "You" : skill.author}</span>
          ),
        },
    {
      id: "updated",
      header: "Updated",
      className: "w-[15%] whitespace-nowrap text-xs text-muted-foreground",
      cell: (skill) => <LocalTime value={skill.updated_at} format="compact" />,
    },
  ];

  return (
    <>
      <SkillTabs tab={tab} q={q} />
      <PageHeader
        title={copy.title}
        documentTitle="Skills"
        subtitle={copy.subtitle}
        actions={
          <Button asChild>
            <Link to="/console/skills/new">New Skill</Link>
          </Button>
        }
      />
      <SearchBox key={tab} placeholder={copy.placeholder} label="Search skills" />
      {list.isError ? (
        // Never let a failed load read as "no skills" or "no matches".
        <ErrorAlert>Could not load skills: {errorMessage(list.error)}</ErrorAlert>
      ) : (
        <DataTable
          fixed
          caption={copy.title}
          columns={columns}
          rows={list.data}
          loading={list.isLoading}
          rowKey={(skill) => skill.id}
          empty={q ? `No skills match “${q}”.` : copy.empty}
        />
      )}
    </>
  );
}
