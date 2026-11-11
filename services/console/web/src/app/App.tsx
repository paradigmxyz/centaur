import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { RouterProvider, type createBrowserRouter } from "react-router";

import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ApiError } from "@/lib/api";
import { applyTheme, watchSystemTheme } from "@/lib/theme";

export function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        // Client errors (401/403/404/422) are final; retry only transient failures.
        retry: (failureCount, error) =>
          !(error instanceof ApiError && error.status >= 400 && error.status < 500) && failureCount < 2,
        refetchOnWindowFocus: false,
      },
    },
  });
}

export function App({ router }: { router: ReturnType<typeof createBrowserRouter> }) {
  const [queryClient] = useState(createQueryClient);

  useEffect(() => {
    applyTheme();
    return watchSystemTheme();
  }, []);

  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <RouterProvider router={router} />
        <Toaster position="top-right" />
      </TooltipProvider>
    </QueryClientProvider>
  );
}
