import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { api, errorMessage } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";

export function DescopeBanner() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const restore = useMutation({
    mutationFn: () => api.delete("/api/v1/descope"),
    onSuccess: async () => {
      queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== sharedKeys.me[0] });
      await queryClient.invalidateQueries({ queryKey: sharedKeys.me });
      toast.success("Admin permissions restored.");
      navigate("/console/principals");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });

  return (
    <div
      role="status"
      className="flex flex-none items-center justify-center gap-4 bg-warning-banner px-4 py-2 text-xs font-bold text-warning-banner-foreground"
    >
      <span>Admin permissions paused — viewing the console as an operator</span>
      <button
        type="button"
        disabled={restore.isPending}
        onClick={() => restore.mutate()}
        className="cursor-pointer rounded-sm border border-warning-banner-foreground bg-warning-banner-foreground px-2.5 py-0.5 font-bold text-amber-400 transition-opacity hover:opacity-85"
      >
        Restore admin
      </button>
    </div>
  );
}
