import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
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
      className="flex flex-none flex-wrap items-center justify-center gap-x-4 gap-y-2 border-b border-warning/30 bg-warning/10 px-4 py-2 text-sm font-medium text-warning"
    >
      <span>Admin permissions paused — viewing the console as an operator</span>
      <Button
        type="button"
        size="xs"
        variant="outline"
        disabled={restore.isPending}
        onClick={() => restore.mutate()}
        className="border-warning/40 bg-transparent text-warning hover:bg-warning/15 hover:text-warning dark:bg-transparent"
      >
        Restore admin
      </Button>
    </div>
  );
}
