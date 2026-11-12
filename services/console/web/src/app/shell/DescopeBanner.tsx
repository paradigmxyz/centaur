import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { restoreAdmin } from "@/app/session";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";

export function DescopeBanner() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [pending, setPending] = useState(false);
  const restore = async () => {
    setPending(true);
    try {
      await restoreAdmin(queryClient);
      toast.success("Admin permissions restored.");
      navigate("/console/principals");
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setPending(false);
    }
  };

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
        disabled={pending}
        onClick={() => void restore()}
        className="border-warning/40 bg-transparent text-warning hover:bg-warning/15 hover:text-warning dark:bg-transparent"
      >
        Restore admin
      </Button>
    </div>
  );
}
